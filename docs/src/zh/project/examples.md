# 可复现示例

在仓库根目录运行 `just examples-pchronicle` 或 `just examples`。
确定性 CLI 示例覆盖 Dataset 生命周期、内置分析、跨 Dataset SQL、存储性能、
格式往返以及直接查询 OpenAI/ACTF。

每个示例维护自己的 `.work/` 目录，并打印可检查的输出。
套件构建 release 可执行文件，需要 Cargo、Python 3、`jq` 和常见 POSIX 工具。

参见 [pChronicle 指南](../pchronicle/guides/index.md)，以及仓库中的
`examples/pchronicle/` 与 `examples/data/`。
