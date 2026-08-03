#!/usr/bin/env python3
import json
import os
import urllib.request

from dialogue_fixture import TURNS

base_url = os.environ["OPENAI_BASE_URL"].rstrip("/")
messages = []
for user_text in TURNS:
    messages.append({"role": "user", "content": user_text})
    request = urllib.request.Request(
        f"{base_url}/chat/completions",
        data=json.dumps({"model": "mock-model", "messages": messages}).encode(),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(request, timeout=10) as response:
        assistant_text = json.load(response)["choices"][0]["message"]["content"]
    messages.append({"role": "assistant", "content": assistant_text})
