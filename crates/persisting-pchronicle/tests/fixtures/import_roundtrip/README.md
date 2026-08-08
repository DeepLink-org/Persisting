# Import round-trip fixtures

These small fixtures are trimmed and sanitized from the large examples under
`data/`. They preserve the relevant JSON shapes while omitting original task
content, command output, environment metadata, and credentials.

- `cybergym_07270003_trimmed.json`: OpenAI-message row with content parts.
- `cybergym_0729001_trimmed.json`: out-of-order rows from multiple sessions.
- `make-doom-for-mips_trimmed.actf.json`: ACTF `tool_use` / `tool_result` shape.
- `protein-assembly_trimmed.actf.json`: ACTF `command_execution` shape with
  nullable token metrics and extension fields.
