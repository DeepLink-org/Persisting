#!/usr/bin/env python3
import copy
import json
import sys
from pathlib import Path

fixture_dir, output, replicas = Path(sys.argv[1]), Path(sys.argv[2]), int(sys.argv[3])
fixtures = [json.loads(path.read_text()) for path in sorted(fixture_dir.glob("*.json"))]

with output.open("w", encoding="utf-8") as stream:
    for replica in range(replicas):
        for fixture in fixtures:
            trajectory = copy.deepcopy(fixture)
            original = trajectory["session_id"]
            trajectory["session_id"] = f"example-{replica:04}-{original}"
            trajectory["trajectory_id"] = f"trajectory-{replica:04}-{original}"
            stream.write(json.dumps(trajectory, separators=(",", ":")) + "\n")
