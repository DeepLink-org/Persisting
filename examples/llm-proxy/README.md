# LLM proxy configs

TOML for `pvisor` / `traj proxy` (`-c`).

| File | Notes |
|------|--------|
| [deepseek.toml](deepseek.toml) | DeepSeek OpenAI + Anthropic dual upstream (default `public`) |
| [multi-provider.toml](multi-provider.toml) | Route by model prefix (DeepSeek / Claude / Gemini / GPT) |
| [allowlist.toml](allowlist.toml) | Same DeepSeek routing with Harbor-style `[network] mode = "allowlist"` |

`allowed_hosts` should list **one host per line**. In `allowlist` mode, hosts from `[[models]].upstream` / `upstream_anthropic` are merged automatically.
