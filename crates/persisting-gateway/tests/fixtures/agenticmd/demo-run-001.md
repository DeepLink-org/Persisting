---
format: persisting:1.0
block: |+
  <!-- persisting:block:{speaker} {json} -->

  message body

session_id: demo-run-001
agent_id: demo-agent
turn_count: 1
---

<!-- persisting:block:user {"type":"markdown","length":6,"call_id":"call-demo-1","event_seq":1,"kind":"llm.request","model":"deepseek-chat","path":"/v1/chat/completions","producer":"persisting-proxy","session_id":"demo-run-001","source":"user","step_id":1,"timestamp":"2026-01-01T00:00:00Z","v":1} -->

你好

<!-- persisting:block:agent {"type":"markdown","length":36,"call_id":"call-demo-1","completion_tokens":18,"event_seq":2,"kind":"llm.response","producer":"persisting-proxy","prompt_tokens":12,"session_id":"demo-run-001","source":"agent","status":200,"step_id":2,"timestamp":"2026-01-01T00:00:01Z","total_tokens":30,"trace_id":"trace-demo-1","v":1} -->

你好！有什么可以帮你的？
