#!/usr/bin/env python3
"""Exercise Gateway through the official OpenAI, Anthropic, and Gemini SDKs."""

from __future__ import annotations

import argparse
import base64
import json
from pathlib import Path
from typing import Any

import anthropic
import openai
from anthropic import Anthropic
from google import genai
from google.genai import types
from openai import OpenAI


def append_result(output: Path, result: dict[str, Any]) -> None:
    with output.open("a", encoding="utf-8") as handle:
        json.dump(result, handle, ensure_ascii=False, sort_keys=True)
        handle.write("\n")


def result(
    *,
    client: str,
    sdk_version: str,
    protocol: str,
    session_id: str,
    model: str,
    input_text: str,
    output_text: str,
    response_kind: str,
    upstream_path: str,
    forward_to: str | None = None,
    streaming: bool = False,
    response_model: str | None = None,
) -> dict[str, Any]:
    return {
        "client": client,
        "sdk_version": sdk_version,
        "protocol": protocol,
        "session_id": session_id,
        "model": model,
        "input": input_text,
        "output": output_text,
        "streaming": streaming,
        "response_model": response_model,
        "expected_capture": {
            "provider": {"openai": "openai", "anthropic": "anthropic", "google-genai": "gemini"}[
                client
            ],
            "response_kind": response_kind,
            "upstream_path": upstream_path,
            "forward_to": forward_to,
        },
    }


def run_openai(gateway: str, output: Path) -> None:
    client = OpenAI(api_key="regression", base_url=f"{gateway}/v1", max_retries=0)
    try:
        chat_session = "sdk-openai-chat"
        chat_input = 'OpenAI chat: "quoted" line\n第二行🙂'
        chat = client.chat.completions.create(
            model="sdk-openai-chat-model",
            messages=[{"role": "user", "content": chat_input}],
            extra_headers={"x-persisting-session-id": chat_session},
        )
        append_result(
            output,
            result(
                client="openai",
                sdk_version=openai.__version__,
                protocol="chat_completions",
                session_id=chat_session,
                model="sdk-openai-chat-model",
                input_text=chat_input,
                output_text=chat.choices[0].message.content or "",
                response_model=chat.model,
                response_kind="llm.response",
                upstream_path="/v1/chat/completions",
                forward_to="echo-openai",
            ),
        )

        responses_session = "sdk-openai-responses-stream"
        responses_input = "OpenAI Responses streaming payload"
        chunks: list[str] = []
        with client.responses.stream(
            model="sdk-openai-responses-model",
            input=responses_input,
            extra_headers={"x-persisting-session-id": responses_session},
        ) as stream:
            for event in stream:
                if event.type == "response.output_text.delta":
                    chunks.append(event.delta)
            final_response = stream.get_final_response()
        append_result(
            output,
            result(
                client="openai",
                sdk_version=openai.__version__,
                protocol="responses",
                session_id=responses_session,
                model="sdk-openai-responses-model",
                input_text=responses_input,
                output_text="".join(chunks),
                response_model=final_response.model,
                response_kind="llm.response.stream",
                upstream_path="/v1/chat/completions",
                forward_to="echo-openai",
                streaming=True,
            ),
        )
    finally:
        client.close()


def run_anthropic(gateway: str, output: Path) -> None:
    session_id = "sdk-anthropic-messages-base64"
    input_text = "Anthropic Messages 中文/Base64"
    client = Anthropic(api_key="regression", base_url=gateway, max_retries=0)
    try:
        message = client.messages.create(
            model="sdk-anthropic-model",
            max_tokens=64,
            messages=[{"role": "user", "content": input_text}],
            extra_headers={
                "x-persisting-session-id": session_id,
                "x-persisting-echo-encoding": "base64",
            },
        )
        output_text = "".join(block.text for block in message.content if block.type == "text")
        assert output_text == base64.b64encode(input_text.encode()).decode(), output_text
        append_result(
            output,
            result(
                client="anthropic",
                sdk_version=anthropic.__version__,
                protocol="messages",
                session_id=session_id,
                model="sdk-anthropic-model",
                input_text=input_text,
                output_text=output_text,
                response_model=message.model,
                response_kind="llm.response",
                upstream_path="/v1/messages",
            ),
        )
    finally:
        client.close()


def run_google_genai(gateway: str, output: Path) -> None:
    session_id = "sdk-google-genai"
    input_text = "Gemini native generateContent payload"
    client = genai.Client(
        api_key="regression",
        http_options=types.HttpOptions(
            base_url=gateway,
            api_version="v1beta",
            headers={"x-persisting-session-id": session_id},
        ),
    )
    try:
        chat = client.chats.create(model="sdk-google-genai-model")
        response = chat.send_message(input_text)
        append_result(
            output,
            result(
                client="google-genai",
                sdk_version=genai.__version__,
                protocol="gemini",
                session_id=session_id,
                model="sdk-google-genai-model",
                input_text=input_text,
                output_text=response.text or "",
                response_model=response.model_version,
                response_kind="llm.response",
                upstream_path="/v1beta/models/sdk-google-genai-model:generateContent",
            ),
        )
    finally:
        client.close()


def run_all(gateway: str, output: Path) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    output.unlink(missing_ok=True)
    gateway = gateway.rstrip("/")
    run_openai(gateway, output)
    run_anthropic(gateway, output)
    run_google_genai(gateway, output)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--gateway", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()

    run_all(args.gateway, args.output)


if __name__ == "__main__":
    main()
