# 设计原则

这些原则解释了为什么 Persisting 拆分为两个产品，也解释了文档为什么强调可审查的步骤。

## 边界必须明确

pVisor 描述实际安装的执行边界；pChronicle 描述实际观察到的 Source 和 Dataset。任何一个产品
都不会把缺失的控制或不完整的 Source 默认为更强的保证。

## 审查前的写入应可逆

Agent 的 Effect 在人或明确策略应用前保持 staged。审查是工作流的一部分，不是写入完成后才补上的报告。

## 结果必须带着证据走

汇总应该能回到产生它的 Run、Dataset、Source 或 query。只有在导出、规范化和再次检查之后仍保留 lineage，
它才真正有用。

## 执行和历史保持可组合

pVisor 可以不依赖 pChronicle 运行，pChronicle 也可以分析外部 Source。集成采用窄化的 capture 契约，
让每个产品单独使用时仍然有价值。

## 可移植数据优先于特权查看器

Dataset、查询结果和 Run 记录应该可以通过 CLI 和公开格式检查。Web 界面可以改善发现，但不应成为恢复答案的唯一方式。

参见[系统概览](index.md)和[路线图](../roadmap.md)。
