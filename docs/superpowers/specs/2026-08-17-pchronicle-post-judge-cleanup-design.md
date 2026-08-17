# pChronicle Judge 删除后的清理设计

## 目标

完成 pChronicle 删除 Judge 子系统后的收尾清理：删除 Web 中失去消费者的
CSS，使用显式错误替换剩余的生产代码 `unreachable!`，验证所有受支持的非
Search feature 组合，并将分散的 AgenticMD 实现与 Storyline 行模型分别收拢到
清晰的领域目录中。

## 范围

本次变更仅包括：

- 删除 `pchronicle-web/src` 中没有任何静态消费者的 pChronicle Web CSS class
  selector；
- 处理默认 feature 构建中由 `clippy::unreachable` 报告的 4 处生产代码
  `unreachable!`；
- 检查 pChronicle 的纯核心、本地 Lance、默认 S3 和 OSS 构建组合；
- 统一本地 `just` lint 入口与对应的 GitHub Actions lint 步骤；
- 收拢当前分散在六个 crate 区域中的 AgenticMD codec、映射、路径、文件操作、
  转换和投影代码；
- 将 Storyline 逻辑行模型与其 Arrow codec 移到同一个目录。

Search、TTAS、Queue 与 Sampler，以及 `persisting-dlcapt` 均不在本次范围内。
只存在于源码中的 `pc2-token-composition` class 也不在范围内，因为它不是孤儿
CSS selector；删除它或为它补充样式都属于新的 UI 决策。

## Web CSS 清理

清理采用保守的 class token 比对。只有当一个 `.pc2-*` selector 的 class token
在 `pchronicle-web/src` 的全部 Rust 源码中均不存在时，才允许删除它。该规则会
删除 4 个确定的 Judge 残留：`pc2-verdict-grid`、`pc2-verdict`、
`pc2-rubric-list` 和 `pc2-score-track`，并删除旧版 Explorer 布局遗留的其他无
消费者 selector。

本次不会重命名保留的 selector，不会修改任何布局参数，也不会修改源码 markup。
验证阶段会重新执行跨文件 class token 比对，并运行 Web 测试与构建检查。

## 显式错误语义

剩余的每处生产代码 `unreachable!` 都在原抽象边界转为普通错误：

- Projection 同步在遇到非 canonical lineage 时报告不兼容错误；
- 本地 manifest 构建拒绝不受支持的查询格式；
- Catalog projection 绑定报告 source kind 内部不一致；
- OpenAI corpus 恢复拒绝未知的已保留文档类型。

成功路径的行为保持不变。转换完成后，pChronicle panic 门禁在现有
`clippy::unwrap_used` 和 `clippy::expect_used` 之外继续 deny
`clippy::unreachable`。

## Feature 矩阵

本地 lint 入口与 CI 验证以下非 Search 配置：

1. 使用 `--no-default-features` 验证轻量格式与事件表面；
2. 使用 `--no-default-features --features lance-store` 验证本地存储；
3. 使用默认 feature 验证兼容 S3 的产品构建；
4. 使用 `--no-default-features --features oss-store` 验证 OSS 后端。

每种配置都以 warnings deny 和生产 panic lint 开启的方式构建 library。
workspace 级严格 Clippy 仍是第一道门禁。CI 调用与本地相同的
`just lint-rust` recipe，避免维护两份 feature 策略。

## AgenticMD 领域收拢

AgenticMD 当前约有 2,465 行实现，分散在 `formats`、`mapping`、`layout`、
`store`、`convert` 和 `projection` 六处。实现统一迁移到 crate 根部的一个私有
子树，并继续按职责拆分文件：

```text
agenticmd/
├── mod.rs
├── codec.rs
├── body.rs
├── frontmatter.rs
├── validate.rs
├── mapping/
│   ├── mod.rs
│   ├── fields.rs
│   └── text.rs
├── layout.rs
├── fs.rs
├── convert.rs
└── projection.rs
```

`projection.rs` 继续受 `lance-store` feature 门控。本次迁移不改变 wire 格式、
路径规则、映射规则、文件系统行为或投影行为。crate 根继续 re-export 现有公开的
AgenticMD 函数、常量与类型，因此当前 Gateway 和 CLI 消费者保持源码兼容。

不通过兼容 wrapper 保留 `persisting_pchronicle::formats::agenticmd::*` 等旧的
实现导向模块路径。workspace 中没有消费者使用这些路径；crate 仍处于 1.0 之前；
保留这些路径也会把本次需要消除的旧结构永久固化。只要无需新增 wrapper 模块，
`formats`、`convert`、`projection` 和 `store` 可以继续在各自模块根 re-export
领域函数，但不再拥有 AgenticMD 实现文件。

## Storyline 行模型收拢

Storyline 三表逻辑模型从 crate 根部的 `storyline_schema.rs` 移至
`store/storyline/model.rs`。它与 `store/storyline/rows.rs` 保持为两个文件：
`model.rs` 负责规范化、重建和逻辑行类型，`rows.rs` 负责 Arrow schema 与 codec。
本次目标是同域代码同目录，而不是合并成一个上千行文件。

crate 根继续导出 `split_storyline`、`reconstruct_storyline`、`StoryRunRow`、
`StoryStepRow`、`StoryToolCallRow`、`StorylineTables` 和三个表名常量。删除根级
`storyline_schema` 模块，Storyline 存储内部通过同目录模块导入逻辑模型。

## 验收与验证

- CSS 与源码 class token 的差集不再包含任何仅存在于 CSS 的 `.pc2-*` class；
- `clippy::unwrap_used`、`clippy::expect_used` 和 `clippy::unreachable` 在四种
  pChronicle feature 组合中均通过；
- 排除 `persisting-dlcapt` 的 workspace 严格 Clippy 保持通过；
- pChronicle library、pChronicle CLI 和 pChronicle Web 测试保持通过；
- 使用 AgenticMD 根级 API 的 Gateway 测试保持通过；
- `formats`、`mapping`、`layout`、`store`、`convert` 和 `projection` 下不再保留
  AgenticMD 实现文件，只有私有 `agenticmd/` 子树拥有该领域实现；
- `storyline_schema.rs` 被删除，Storyline 模型与 Arrow 代码共同位于
  `store/storyline/`，同时保留根级 item 导出；
- 现有用户未跟踪的评审稿和 RFC 文件保持不变。
