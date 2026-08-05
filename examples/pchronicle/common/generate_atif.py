#!/usr/bin/env python3
import copy
import json
import os
import random
import sys
from pathlib import Path


SOURCE_SUFFIXES = {".rs", ".py", ".sh", ".toml"}
SOURCE_ROOTS = ("crates", "persisting", "examples", "scripts")
EXCLUDED_PARTS = {
    ".git",
    ".work",
    "queue",
    "sampler",
    "search",
    "target",
    "tmp",
    "vendor",
    "persisting-dlcapt",
}
DEFAULT_SEED = 20260805


def find_repo_root(start: Path) -> Path:
    for candidate in (start, *start.parents):
        if (candidate / "Cargo.toml").is_file() and (candidate / "crates").is_dir():
            return candidate
    raise SystemExit(f"cannot find repository root above {start}")


def source_blocks(repo_root: Path) -> tuple[list[tuple[str, str]], int]:
    blocks: list[tuple[str, str]] = []
    seen_blocks: set[str] = set()
    source_files = 0
    for root_name in SOURCE_ROOTS:
        source_root = repo_root / root_name
        if not source_root.is_dir():
            continue
        for path in sorted(source_root.rglob("*")):
            relative = path.relative_to(repo_root)
            if (
                not path.is_file()
                or path.suffix not in SOURCE_SUFFIXES
                or EXCLUDED_PARTS.intersection(relative.parts)
            ):
                continue
            try:
                text = path.read_text(encoding="utf-8")
            except UnicodeDecodeError:
                continue
            source_files += 1
            # Blank-line-delimited blocks usually correspond to functions,
            # declarations, tests, or shell sections and remain meaningful
            # when sampled as trajectory text.
            for block in text.split("\n\n"):
                normalized = "\n".join(line.rstrip() for line in block.splitlines()).strip()
                if len(normalized) < 40:
                    continue
                for offset in range(0, len(normalized), 1200):
                    chunk = normalized[offset : offset + 1200]
                    if len(chunk) >= 40 and chunk not in seen_blocks:
                        seen_blocks.add(chunk)
                        blocks.append((relative.as_posix(), chunk))
    if not blocks:
        raise SystemExit(f"no source text found below {repo_root}")
    return blocks, source_files


class SourceSampler:
    def __init__(self, blocks: list[tuple[str, str]], seed: int):
        self.blocks = blocks
        self.random = random.Random(seed)
        self.random.shuffle(self.blocks)
        self.cursor = 0

    def excerpt(self, target_chars: int) -> str:
        parts: list[str] = []
        chars = 0
        while chars < target_chars:
            if self.cursor == len(self.blocks):
                self.random.shuffle(self.blocks)
                self.cursor = 0
            path, block = self.blocks[self.cursor]
            self.cursor += 1
            piece = f"{block}\n// source: {path}\n"
            parts.append(piece)
            chars += len(piece)
        return "".join(parts)[:target_chars]


def sampled_message(
    original: str, prefix: str, sampler: SourceSampler, seen_messages: set[str]
) -> str:
    body_chars = max(0, len(original) - len(prefix))
    for _ in range(64):
        candidate = (prefix + sampler.excerpt(body_chars))[: len(original)]
        if candidate not in seen_messages:
            seen_messages.add(candidate)
            return candidate
    raise SystemExit("source corpus cannot produce enough distinct message text")


def replace_message_text(
    step: dict, sampler: SourceSampler, seen_messages: set[str]
) -> None:
    message = step.get("message")
    prefix = f"{step.get('source', 'unknown')} step {step.get('step_id', '?')}: "
    if isinstance(message, str) and message:
        step["message"] = sampled_message(message, prefix, sampler, seen_messages)
        return
    if not isinstance(message, list):
        return
    for content in message:
        if not isinstance(content, dict):
            continue
        text = content.get("text")
        if isinstance(text, str) and text:
            content["text"] = sampled_message(text, prefix, sampler, seen_messages)


if len(sys.argv) != 4:
    raise SystemExit("usage: generate_atif.py FIXTURE_DIR OUTPUT REPLICAS")

fixture_dir, output, replicas = Path(sys.argv[1]), Path(sys.argv[2]), int(sys.argv[3])
if replicas <= 0:
    raise SystemExit("REPLICAS must be greater than zero")
fixtures = [json.loads(path.read_text()) for path in sorted(fixture_dir.glob("*.json"))]
if not fixtures:
    raise SystemExit(f"no ATIF fixtures found in {fixture_dir}")

seed = int(os.environ.get("PCHRONICLE_CORPUS_SEED", DEFAULT_SEED))
repo_root = find_repo_root(fixture_dir.resolve())
blocks, source_file_count = source_blocks(repo_root)
sampler = SourceSampler(blocks, seed)
seen_messages: set[str] = set()

with output.open("w", encoding="utf-8") as stream:
    for replica in range(replicas):
        for fixture in fixtures:
            trajectory = copy.deepcopy(fixture)
            original = trajectory["session_id"]
            trajectory["session_id"] = f"example-{replica:04}-{original}"
            trajectory["trajectory_id"] = f"trajectory-{replica:04}-{original}"
            for step in trajectory.get("steps", []):
                replace_message_text(step, sampler, seen_messages)
            stream.write(json.dumps(trajectory, separators=(",", ":")) + "\n")

print(
    f"source_corpus_files={source_file_count} source_blocks={len(blocks)} "
    f"seed={seed}"
)
