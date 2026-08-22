"""Pinned SWE-agent 1.1.0 replay-then-live bridge, launched by Rust."""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path
from typing import Any

from sweagent.agent.agents import DefaultAgent
from sweagent.environment.swe_env import SWEEnv
from sweagent.run.run_single import RunSingleConfig
from swerex.deployment.config import get_deployment


class ReplayThenLiveModel:
    def __init__(self, prefix: list[dict[str, Any]], live_model: Any) -> None:
        self.prefix = prefix
        self.live_model = live_model
        self.index = 0
        self.live_calls = 0

    @property
    def stats(self) -> Any:
        return self.live_model.stats

    @property
    def config(self) -> Any:
        return self.live_model.config

    def query(self, history: list[dict[str, Any]], **kwargs: Any) -> dict[str, Any]:
        if self.index < len(self.prefix):
            value = self.prefix[self.index]
            self.index += 1
            result: dict[str, Any] = {"message": str(value.get("content") or "")}
            if value.get("tool_calls") is not None:
                result["tool_calls"] = value["tool_calls"]
            if value.get("thinking_blocks") is not None:
                result["thinking_blocks"] = value["thinking_blocks"]
            return result
        self.live_calls += 1
        return self.live_model.query(history, **kwargs)

    def __getattr__(self, name: str) -> Any:
        return getattr(self.live_model, name)


def _config(raw: Any, workspace: str) -> RunSingleConfig:
    if isinstance(raw, str):
        raw = json.loads(raw)
    if not isinstance(raw, dict):
        raise TypeError("trajectory replay_config must be an object or encoded object")
    value = json.loads(json.dumps(raw))
    value.setdefault("env", {})
    value["env"]["deployment"] = {"type": "local"}
    value["env"]["repo"] = {"type": "preexisting", "repo_name": workspace}
    model = value.setdefault("agent", {}).setdefault("model", {})
    model["api_key"] = os.environ["OPENAI_API_KEY"]
    model["api_base"] = os.environ["OPENAI_BASE_URL"]
    model_name = os.environ.get("MODEL_NAME") or os.environ.get("LLM_MODEL")
    if model_name:
        model["name"] = model_name
    return RunSingleConfig.model_validate(value)


def main() -> int:
    if len(sys.argv) != 2:
        raise SystemExit("runner expects one JSON request path")
    request = json.loads(Path(sys.argv[1]).read_text())
    mode = str(request["mode"])
    if mode not in {"replay_only", "replay_and_continue"}:
        raise ValueError(f"unsupported replay mode: {mode}")
    after_step = int(request["after_step"])
    max_steps = request.get("max_steps")
    if max_steps is not None:
        max_steps = int(max_steps)
        if max_steps < after_step or (mode == "replay_and_continue" and max_steps == after_step):
            raise ValueError("max_steps does not leave the steps required by replay mode")
    source = json.loads(Path(request["trajectory"]).read_text())
    config = _config(source.get("replay_config"), request["workspace"])
    if getattr(config.agent, "type", None) != "default":
        raise ValueError("SWE-agent retry configurations are unsupported for deterministic replay")
    assistant = [item for item in source["history"] if item.get("role") == "assistant"]
    prefix = assistant[:after_step]
    if len(prefix) != after_step:
        raise ValueError("SWE-agent history has fewer assistant actions than the cutoff")

    agent = DefaultAgent.from_config(config.agent)
    live_model = agent.model
    agent.model = ReplayThenLiveModel(prefix, live_model)
    agent.replay_config = config
    environment = SWEEnv(
        deployment=get_deployment(config.env.deployment),
        repo=config.env.repo,
        post_startup_commands=[],
    )
    output_dir = Path(request["output_dir"])
    agent.setup(
        env=environment,
        problem_statement=config.problem_statement,
        output_dir=output_dir,
    )
    agent.replay_config = config
    total_steps = 0
    done = False
    while True:
        if mode == "replay_only" and total_steps >= after_step:
            break
        if max_steps is not None and total_steps >= max_steps:
            break
        step_output = agent.step()
        total_steps += 1
        agent.save_trajectory()
        if total_steps == after_step:
            Path(request["reconstructed"]).write_text(
                json.dumps(agent.get_trajectory_data(), indent=2) + "\n"
            )
        if total_steps >= after_step and step_output.done:
            done = True
            break
    if agent.model.index != after_step:
        raise RuntimeError(
            f"SWE-agent replay consumed {agent.model.index} source actions, expected {after_step}"
        )
    if mode == "replay_only" and agent.model.live_calls != 0:
        raise RuntimeError("SWE-agent replay-only unexpectedly queried the live model")
    data = agent.get_trajectory_data()
    agent.save_trajectory()
    continued_steps = max(0, total_steps - after_step)
    if mode == "replay_only":
        phase = "replayed"
        agent_status = "not_started"
        trajectory_path = Path(request["reconstructed"])
    else:
        phase = "continued"
        agent_status = "completed" if done else "max_steps"
        trajectory_path = Path(request["continued"])
        trajectory_path.write_text(json.dumps(data, indent=2) + "\n")
    Path(request["result"]).write_text(
        json.dumps(
            {
                "phase": phase,
                "agent_status": agent_status,
                "replayed_steps": after_step,
                "continued_steps": continued_steps,
                "trajectory": str(trajectory_path),
                "reconstructed": str(request["reconstructed"]),
                "trajectory_steps": len(data["trajectory"]),
            },
            indent=2,
        )
        + "\n"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
