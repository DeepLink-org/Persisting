# 轨迹存储

轨迹处理模型为 `StorylineDocument`。支持 ATIF、ACTF、OpenAI Messages、
Storyline JSON 与 AgenticMD；Codex 和 Claude Code 会话 JSONL 只支持导入。
编解码器继续解析并保留这些格式的时间戳。

`StorylineLanceStore` 直接把文档写入 `runs.lance`、`steps.lance` 和
`tool_calls.lance`，大字段使用内容层。`CURRENT` 固定已发布 generation 和精确
Lance 版本。文档重建读取固定版本并校验引用内容。

导入支持创建、追加与替换。本地整 Dataset 替换先暂存输出再交换目录；对象存储
整 Dataset 替换的失败语义不同，详见导入指南。store 内部发布使用写入租约与
 generation 校验。维护与删除遵守同样的发布与内容完整性保证。

Compact JSONL 是独立的记录存储路径，保留原始 JSON 字节，不推断 Storyline
Run、Step 或 ToolCall。

参见 [Storyline Lance](storyline-lance.md)、[Snapshot](catalog.md)、
[格式](../reference/formats/index.md) 与 [导入导出](../guides/exchange.md)。
