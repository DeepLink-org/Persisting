"""Trajectory replay JSON row shape (no Rust extension required)."""

from __future__ import annotations

import json
import unittest


def _parse_replay_record(line: str) -> dict:
    row = json.loads(line.strip())
    if not isinstance(row, dict):
        raise ValueError("replay record must be a JSON object")
    return row


class TestTrajectoryDialogue(unittest.TestCase):
    def test_markdown_replay_row_shape(self) -> None:
        row = _parse_replay_record(
            '{"type":"markdown","role":"user","kind":"llm.request","content":"你好","turn":1}'
        )
        self.assertEqual(row["type"], "markdown")
        self.assertEqual(row["role"], "user")
        self.assertEqual(row["content"], "你好")
        self.assertEqual(row["turn"], 1)

    def test_rejects_non_object(self) -> None:
        with self.assertRaises(ValueError):
            _parse_replay_record("[1,2]")


if __name__ == "__main__":
    unittest.main()
