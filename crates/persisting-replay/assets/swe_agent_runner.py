"""Pinned SWE-agent 1.1.0 replay-then-live bridge, launched by Rust."""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path
from typing import Any

from sweagent.agent.agents import DefaultAgent
from sweagent.environment.swe_env import SWEEnv
from sweagent.run.run_single import RunSingle, RunSingleConfig
from swerex.deployment.config import get_deployment


class ReplayThenLiveModel:
    def __init__(self, prefix: list[dict[str, Any]], live_model: Any) -> None:
        self.prefix = prefix
        self.live_model = live_model
        self.index = 0

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
    source = json.loads(Path(request["trajectory"]).read_text())
    config = _config(source.get("replay_config"), request["workspace"])
    assistant = [item for item in source["history"] if item.get("role") == "assistant"]
    prefix = assistant[: int(request["after_step"])]
    if len(prefix) != int(request["after_step"]):
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
    run = RunSingle(
        environment,
        agent,
        problem_statement=config.problem_statement,
        output_dir=Path(request["output_dir"]),
    )
    run.run()
    print(json.dumps({"status": "completed", "replayed_actions": len(prefix)}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
