---
format: persisting:1.0
block: |+
  <!-- persisting:block:{speaker} {json} -->

  message body

---

<!-- persisting:block:user {"type":"markdown","length":6,"kind":"llm.request","model":"deepseek-chat","path":"/v1/chat/completions","role":"user","seq":0,"session_id":"demo-run-001","source":"persisting-proxy","timestamp":"2026-01-01T00:00:00Z","turn":1,"v":1} -->

你好

<!-- persisting:block:assistant {"type":"markdown","length":36,"call_id":"call-demo-1","completion_tokens":18,"kind":"llm.response","prompt_tokens":12,"role":"assistant","seq":0,"session_id":"demo-run-001","source":"persisting-proxy","status":200,"timestamp":"2026-01-01T00:00:00Z","total_tokens":30,"trace_id":"trace-demo-1","turn":1,"v":1} -->

你好！有什么可以帮你的？

<!-- persisting:block:note {"type":"markdown","length":89,"kind":"note","role":"note","seq":2,"session_id":"demo-run-001","source":"markdown","timestamp":"2026-01-01T00:00:00Z","turn":2,"v":1} -->

本次会话由 `persisting capture run` 代理记录；元数据在块注释 JSON 中。

