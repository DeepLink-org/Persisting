"""Pinned mini-swe-agent 2.4.6 replay bridge, launched by the Rust engine."""

from __future__ import annotations

import copy
import json
import os
import sys
import time
from pathlib import Path
from typing import Any

SESSION_HEADER_NAME = "X-LiteLLM-Session-ID"


def _load(path: Path) -> dict[str, Any]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise TypeError(f"{path} must contain an object")
    return value


def _action_messages(messages: list[dict[str, Any]]) -> list[tuple[int, dict[str, Any]]]:
    result = []
    for index, message in enumerate(messages):
        extra = message.get("extra")
        if isinstance(extra, dict) and isinstance(extra.get("actions"), list) and extra["actions"]:
            result.append((index, message))
    return result


def _has_model_response(message: dict[str, Any]) -> bool:
    extra = message.get("extra")
    return isinstance(extra, dict) and isinstance(extra.get("response"), dict)


def _preserved_messages_between_actions(
    messages: list[dict[str, Any]],
    *,
    previous_index: int,
    next_index: int,
    observation_count: int,
) -> list[dict[str, Any]]:
    preserved: list[dict[str, Any]] = []
    skipped_observations = 0
    for message in messages[previous_index + 1 : next_index]:
        extra = message.get("extra")
        is_interrupt = isinstance(extra, dict) and bool(extra.get("interrupt_type"))
        is_observation = (
            message.get("role") in {"tool", "user"}
            or message.get("type") == "function_call_output"
        )
        if skipped_observations < observation_count and is_observation and not is_interrupt:
            skipped_observations += 1
            continue
        preserved.append(copy.deepcopy(message))
    if skipped_observations != observation_count:
        raise ValueError("native trajectory does not contain all observations for the previous action")
    return preserved


def _fresh_observation(
    action: dict[str, Any], output: dict[str, Any], duration_ms: int
) -> dict[str, Any]:
    return {
        "call_id": str(action.get("tool_call_id") or ""),
        "content": output.get("output", ""),
        "is_error": bool(output.get("exception_info")) or output.get("returncode") != 0,
        "return_code": output.get("returncode"),
        "duration_ms": duration_ms,
    }


def _continue(agent: Any, output_path: Path) -> None:
    from minisweagent.exceptions import FormatError, InterruptAgentFlow

    while not agent.messages or agent.messages[-1].get("role") != "exit":
        try:
            agent.step()
            agent.n_consecutive_format_errors = 0
        except FormatError as exc:
            agent.cost += exc.messages[0].get("extra", {}).get("cost", 0.0)
            agent.n_consecutive_format_errors += 1
            limit = agent.config.max_consecutive_format_errors
            if 0 < limit <= agent.n_consecutive_format_errors:
                agent.add_messages(
                    *exc.messages,
                    {
                        "role": "exit",
                        "content": "RepeatedFormatError",
                        "extra": {"exit_status": "RepeatedFormatError", "submission": ""},
                    },
                )
            else:
                agent.add_messages(*exc.messages)
        except InterruptAgentFlow as exc:
            agent.add_messages(*exc.messages)
        except Exception as exc:
            agent.handle_uncaught_exception(exc)
            raise
        finally:
            agent.save(output_path)


def run(request: dict[str, Any]) -> None:
    from minisweagent.agents import get_agent
    from minisweagent.environments import get_environment
    from minisweagent.models import get_model

    source = _load(Path(request["source"]))
    info = source["info"]
    config = copy.deepcopy(info["config"])
    messages = source["messages"]
    selected = _action_messages(messages)[: int(request["after_step"])]
    if len(selected) != int(request["after_step"]):
        raise ValueError("native trajectory does not contain the requested replay prefix")

    model_config = config["model"]
    model_type = config.get("model_type")
    if model_type:
        model_config["model_class"] = model_type
    if os.environ.get("MODEL_NAME"):
        model_config["model_name"] = os.environ["MODEL_NAME"]
    model_config["cost_tracking"] = "ignore_errors"
    model_kwargs = model_config.setdefault("model_kwargs", {})
    headers = {
        str(key): str(value)
        for key, value in (model_kwargs.get("extra_headers") or {}).items()
        if str(key).lower() != SESSION_HEADER_NAME.lower()
    }
    headers[SESSION_HEADER_NAME] = request["session_id"]
    model_kwargs["extra_headers"] = headers

    environment_config = config["environment"]
    environment_type = config.get("environment_type")
    if environment_type:
        environment_config["environment_class"] = environment_type
    environment_config["cwd"] = request["workspace"]

    agent_config = config["agent"]
    agent_type = config.get("agent_type")
    if agent_type:
        agent_config["agent_class"] = agent_type
    agent_config["output_path"] = request["continued"]
    if "mode" in agent_config:
        agent_config["mode"] = "yolo"
    if "confirm_exit" in agent_config:
        agent_config["confirm_exit"] = False
    if request.get("max_steps") is not None:
        agent_config["step_limit"] = int(request["max_steps"])
    agent_config["cost_limit"] = 0

    model = get_model(config=model_config)
    environment = get_environment(environment_config, default_type="local")
    agent = get_agent(model, environment, agent_config, default_type="default")

    first_action_index = selected[0][0]
    agent.messages = copy.deepcopy(messages[:first_action_index])
    fresh: list[dict[str, Any]] = []
    previous: tuple[int, dict[str, Any]] | None = None
    for message_index, original in selected:
        if previous is not None:
            previous_index, previous_message = previous
            agent.add_messages(
                *_preserved_messages_between_actions(
                    messages,
                    previous_index=previous_index,
                    next_index=message_index,
                    observation_count=len(previous_message["extra"]["actions"]),
                )
            )
        assistant = copy.deepcopy(original)
        agent.add_messages(assistant)
        outputs = []
        for action in assistant["extra"]["actions"]:
            started = time.monotonic()
            output = environment.execute(action)
            outputs.append(output)
            fresh.append(_fresh_observation(action, output, int((time.monotonic() - started) * 1000)))
        agent.add_messages(
            *model.format_observation_messages(assistant, outputs, agent.get_template_vars())
        )
        previous = (message_index, original)

    source_prefix = messages[: selected[-1][0] + 1]
    agent.n_calls = sum(_has_model_response(message) for message in source_prefix)
    agent.cost = sum(float((message.get("extra") or {}).get("cost") or 0) for message in source_prefix)
    Path(request["observations"]).write_text(
        json.dumps(fresh, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    _continue(agent, Path(request["continued"]))


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("runner expects one JSON request path")
    run(_load(Path(sys.argv[1])))
