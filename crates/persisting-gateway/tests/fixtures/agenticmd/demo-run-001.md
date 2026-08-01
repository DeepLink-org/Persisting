---
format: persisting:1.0
block: |+
  <!-- persisting:block:{speaker} {json} -->

  message body

session: demo-run-001
agent: demo-agent
turns: 1
---

<!-- persisting:block:user {"type":"markdown","length":6,"call_id":"call-demo-1","kind":"llm.request","model":"deepseek-chat","path":"/v1/chat/completions","role":"user","seq":1,"session_id":"demo-run-001","source":"persisting-proxy","timestamp":"2026-01-01T00:00:00Z","turn":1,"v":1} -->

你好

<!-- persisting:block:assistant {"type":"markdown","length":36,"call_id":"call-demo-1","completion_tokens":18,"kind":"llm.response","prompt_tokens":12,"role":"assistant","seq":2,"session_id":"demo-run-001","source":"persisting-proxy","status":200,"timestamp":"2026-01-01T00:00:01Z","total_tokens":30,"trace_id":"trace-demo-1","turn":2,"v":1} -->

你好！有什么可以帮你的？
