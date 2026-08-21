"""Inject minimal pinned-SDK fakes, then execute one replay runner."""

from __future__ import annotations

import json
import os
import runpy
import sys
import types
from pathlib import Path
from types import SimpleNamespace
from typing import Any


def _module(name: str) -> types.ModuleType:
    module = types.ModuleType(name)
    sys.modules[name] = module
    return module


def _touch(path: str | None, text: str = "1\n") -> None:
    if path:
        target = Path(path)
        target.parent.mkdir(parents=True, exist_ok=True)
        with target.open("a", encoding="utf-8") as stream:
            stream.write(text)


def _install_mini() -> None:
    _module("minisweagent")
    agents = _module("minisweagent.agents")
    environments = _module("minisweagent.environments")
    models = _module("minisweagent.models")
    exceptions = _module("minisweagent.exceptions")

    class FormatError(Exception):
        pass

    class InterruptAgentFlow(Exception):
        pass

    exceptions.FormatError = FormatError
    exceptions.InterruptAgentFlow = InterruptAgentFlow

    class FakeEnvironment:
        def execute(self, action: dict[str, Any]) -> dict[str, Any]:
            _touch(action.get("marker"))
            return {"output": "fresh observation", "returncode": 0}

    class FakeModel:
        def format_observation_messages(
            self,
            assistant: dict[str, Any],
            outputs: list[dict[str, Any]],
            template_vars: dict[str, Any],
        ) -> list[dict[str, Any]]:
            del assistant, template_vars
            return [
                {
                    "role": "tool",
                    "content": output["output"],
                    "extra": {"returncode": output["returncode"]},
                }
                for output in outputs
            ]

    class FakeAgent:
        def __init__(self, model: Any, environment: Any, config: dict[str, Any]) -> None:
            self.model = model
            self.environment = environment
            self.messages: list[dict[str, Any]] = []
            self.n_calls = 0
            self.cost = 0.0
            self.n_consecutive_format_errors = 0
            self.config = SimpleNamespace(
                max_consecutive_format_errors=3,
                step_limit=config.get("step_limit"),
            )

        def add_messages(self, *messages: dict[str, Any]) -> None:
            self.messages.extend(messages)

        def get_template_vars(self) -> dict[str, Any]:
            return {}

        def save(self, path: Path) -> None:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(
                json.dumps({"messages": self.messages, "info": {}}, indent=2) + "\n",
                encoding="utf-8",
            )

        def step(self) -> None:
            if self.config.step_limit is not None and self.n_calls >= self.config.step_limit:
                self.add_messages(
                    {
                        "role": "exit",
                        "content": "LimitsExceeded",
                        "extra": {"exit_status": "LimitsExceeded", "submission": ""},
                    }
                )
                return
            _touch(os.environ.get("FAKE_LIVE_MARKER"))
            self.n_calls += 1
            if os.environ.get("FAKE_NEVER_COMPLETE") == "1":
                self.add_messages({"role": "assistant", "content": "continue", "extra": {}})
            else:
                self.add_messages(
                    {
                        "role": "exit",
                        "content": "Completed",
                        "extra": {"exit_status": "Completed", "submission": "done"},
                    }
                )

        def handle_uncaught_exception(self, exc: Exception) -> None:
            raise exc

    agents.get_agent = lambda model, environment, config, default_type: FakeAgent(
        model, environment, config
    )
    environments.get_environment = lambda config, default_type: FakeEnvironment()
    models.get_model = lambda config: FakeModel()


def _install_swe() -> None:
    for name in [
        "sweagent",
        "sweagent.agent",
        "sweagent.environment",
        "sweagent.run",
        "swerex",
        "swerex.deployment",
    ]:
        _module(name)
    agents = _module("sweagent.agent.agents")
    environment_module = _module("sweagent.environment.swe_env")
    run_single = _module("sweagent.run.run_single")
    deployment = _module("swerex.deployment.config")

    class FakeProblem:
        id = "fake-problem"

        def get_problem_statement(self) -> str:
            return "fake problem"

    class RunSingleConfig:
        @classmethod
        def model_validate(cls, value: dict[str, Any]) -> Any:
            agent_value = value.get("agent") or {}
            return SimpleNamespace(
                agent=SimpleNamespace(type=agent_value.get("type", "default")),
                env=SimpleNamespace(
                    deployment=(value.get("env") or {}).get("deployment"),
                    repo=(value.get("env") or {}).get("repo"),
                ),
                problem_statement=FakeProblem(),
                model_dump=lambda: value,
            )

    class FakeLiveModel:
        stats = SimpleNamespace()
        config = SimpleNamespace()

        def query(self, history: list[dict[str, Any]], **kwargs: Any) -> dict[str, Any]:
            del history, kwargs
            _touch(os.environ.get("FAKE_LIVE_MARKER"))
            return {"message": "live action"}

    class DefaultAgent:
        @classmethod
        def from_config(cls, config: Any) -> "DefaultAgent":
            del config
            instance = cls()
            instance.model = FakeLiveModel()
            instance.trajectory: list[dict[str, Any]] = []
            instance.history: list[dict[str, Any]] = []
            instance.info: dict[str, Any] = {}
            instance.replay_config = None
            instance.traj_path: Path | None = None
            instance._env = None
            return instance

        def setup(self, env: Any, problem_statement: Any, output_dir: Path) -> None:
            self._env = env
            output_dir.mkdir(parents=True, exist_ok=True)
            self.traj_path = output_dir / f"{problem_statement.id}.traj"

        def step(self) -> Any:
            response = self.model.query(self.history)
            action = str(response.get("message") or "")
            self.history.append({"role": "assistant", "content": action})
            self.trajectory.append(
                {"action": action, "observation": f"fresh-{len(self.trajectory) + 1}"}
            )
            return SimpleNamespace(done=False)

        def get_trajectory_data(self) -> dict[str, Any]:
            return {
                "trajectory": self.trajectory,
                "history": self.history,
                "info": self.info,
                "replay_config": None,
                "environment": "fake",
            }

        def save_trajectory(self) -> None:
            assert self.traj_path is not None
            self.traj_path.write_text(
                json.dumps(self.get_trajectory_data(), indent=2) + "\n",
                encoding="utf-8",
            )

    class SWEEnv:
        def __init__(self, deployment: Any, repo: Any, post_startup_commands: list[str]) -> None:
            del deployment, post_startup_commands
            self.repo = SimpleNamespace(repo_name=str(repo))
            self.name = "fake"

    agents.DefaultAgent = DefaultAgent
    environment_module.SWEEnv = SWEEnv
    run_single.RunSingleConfig = RunSingleConfig
    deployment.get_deployment = lambda config: config


def main() -> None:
    if len(sys.argv) != 4:
        raise SystemExit("usage: fake_agent_runtime.py KIND RUNNER REQUEST")
    kind, runner, request = sys.argv[1:]
    if kind == "mini":
        _install_mini()
    elif kind == "swe":
        _install_swe()
    else:
        raise ValueError(f"unknown fake kind: {kind}")
    sys.argv = [runner, request]
    runpy.run_path(runner, run_name="__main__")


if __name__ == "__main__":
    main()
