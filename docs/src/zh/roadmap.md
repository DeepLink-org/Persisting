# 路线图

路线图描述产品方向，不代表每一项都已经实现。[项目](project/index.md)页面和
发布记录才是已交付行为的依据。

## 当前：让本地闭环可靠

- 让 pVisor 的 Run → review → apply 在 macOS 和 Linux 上稳定可预测。
- 在每个 Run Bundle 中清楚展示实际控制机制、Effect 和警告。
- 让 pChronicle 的 onboarding、Dataset 发现、有界 SQL 和 Evidence 定位无需服务
  或账号即可使用。
- 保持中英文文档路径一致，并让示例可以运行。

## 下一步：把执行连接到持久历史

- 让配置好的 pVisor capture 生成稳定的 pChronicle Source。
- 在捕获、规范化和查询之间保留 Run identity 与 lineage。
- 改进 Run、Session 和 revision 的比较流程。
- 记录不同 provider 的边界，不宣称一个统一的隔离强度。

## 之后：从单机走向团队和集群

- 在不隐藏 provenance 的前提下共享 Dataset catalog 和策略。
- 为 host、OCI container 和 VM 提供可复现的执行配置。
- 补齐 retention、访问控制和成本感知存储的运维指南。

## 如何理解路线图

有设计文档不等于功能已经完成。只有在 CLI 路径、测试或示例、限制说明和发布记录
都具备后，才应把功能视为可用。改变数据契约、执行边界或公开命令的改动应先进入 RFC。
