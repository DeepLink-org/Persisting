"""Walkthrough 固定对话 — agent / mock / check 共用，避免文案漂移。"""

TURNS = [
    "你好，请用一句话介绍 Persisting capture。",
    "轨迹 Markdown 文件路径是什么？",
]

REPLIES = [
    (
        "Persisting capture 通过内嵌代理拦截 LLM 调用，"
        "把对话写入 session markdown（默认 `0001.md`）。"
    ),
    (
        "路径为 `{storage}/{agent_id}/{session_id}/0001.md`，"
        "文件头为 YAML frontmatter，每块是 "
        "`<!-- persisting:block:{speaker} {json} -->` + 裸正文。"
    ),
]

REPLY_BY_USER = dict(zip(TURNS, REPLIES, strict=True))
