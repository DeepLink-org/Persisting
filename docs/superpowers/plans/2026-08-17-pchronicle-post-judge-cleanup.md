# pChronicle Judge 删除后的清理实施计划

> **供 agentic worker 使用：** 必须使用 `superpowers:subagent-driven-development`（推荐）或 `superpowers:executing-plans`，逐项执行本计划。所有步骤使用 checkbox 跟踪。

**目标：** 清理 pChronicle Web 孤儿 CSS，消除剩余生产 `unreachable!`，补齐非 Search feature 门禁，并收拢 AgenticMD 与 Storyline 行模型的物理目录边界。

**架构：** 所有改动保持现有根级公共 item API 和运行时语义。AgenticMD 实现统一迁入私有 `agenticmd/` 子树；Storyline 逻辑模型迁入 `store/storyline/model.rs` 并与 Arrow codec 并列。CI 只调用本地 `just lint-rust`，由该 recipe 定义完整 feature 矩阵。

**技术栈：** Rust 2021、Cargo、Clippy、Dioxus Web、GitHub Actions、Just。

## 全局约束

- 不进入 Search、TTAS、Queue、Sampler 或 `persisting-dlcapt`。
- 不修改、删除或提交用户已有的未跟踪文件。
- 不改变 AgenticMD wire、路径、映射、文件或投影语义。
- 保留当前 crate 根级 AgenticMD 和 Storyline item 导出。
- 不为旧的深层实现路径增加兼容 wrapper。
- 每个结构迁移先运行已有行为测试建立绿色基线，迁移后运行同一测试；不提交只检测私有文件路径的 change-detector 测试。

---

### 任务 1：删除 Web 孤儿 CSS

**文件：**
- 修改：`pchronicle-web/assets/analysis.css`
- 修改：`pchronicle-web/assets/path-explorer.css`
- 修改：`pchronicle-web/assets/workbench.css`

**接口：**
- 输入：`pchronicle-web/src` 中出现的静态 `pc2-*` class token。
- 输出：CSS 中不存在无源码消费者的 `.pc2-*` selector。

- [ ] **步骤 1：运行 class 差集检查，确认 RED**

运行：

```bash
comm -23 \
  <(rg --no-filename -o '\.pc2-[a-z0-9-]+' pchronicle-web/assets/*.css | sed 's/^\.//' | sort -u) \
  <(rg --no-filename -o 'pc2-[a-z0-9-]+' pchronicle-web/src | sort -u)
```

预期：输出 25 个 CSS-only class，其中包括 `pc2-verdict-grid`、
`pc2-verdict`、`pc2-rubric-list` 和 `pc2-score-track`。

- [ ] **步骤 2：只删除 RED 列表中的 selector 规则**

从三个 CSS 文件删除所有引用 RED class 的完整规则；组合 selector 中只删除对应
分支，保留仍有消费者的 selector 和声明。不得修改 Rust markup，也不得删除
source-only 的 `pc2-token-composition`。

- [ ] **步骤 3：运行 class 差集检查，确认 GREEN**

重复步骤 1 的命令。

预期：无输出。

- [ ] **步骤 4：验证 Web**

运行：

```bash
cargo fmt --manifest-path pchronicle-web/Cargo.toml -- --check
cargo test --manifest-path pchronicle-web/Cargo.toml --locked
```

预期：格式检查和全部 Web 测试通过。

- [ ] **步骤 5：提交**

```bash
git add pchronicle-web/assets/analysis.css \
  pchronicle-web/assets/path-explorer.css \
  pchronicle-web/assets/workbench.css
git commit -m "style: remove orphaned pchronicle web selectors"
```

### 任务 2：消除生产 `unreachable!`

**文件：**
- 修改：`crates/persisting-pchronicle/src/projection/storyline.rs`
- 修改：`crates/persisting-pchronicle/src/store/local_query_manifest.rs`
- 修改：`crates/persisting-pchronicle/src/store/catalog/discovery.rs`
- 修改：`crates/persisting-pchronicle/src/formats/openai_corpus.rs`

**接口：**
- 输入：现有四处内部不变量。
- 输出：同一函数的普通 `Result` 错误，不再触发进程 panic。

- [ ] **步骤 1：运行严格 lint，确认 RED**

运行：

```bash
cargo clippy -p persisting-pchronicle --lib --locked -- \
  -D warnings \
  -D clippy::unwrap_used \
  -D clippy::expect_used \
  -D clippy::unreachable
```

预期：仅因四处 `unreachable!` 失败。

- [ ] **步骤 2：将 Projection lineage 分支改为显式错误**

在 `sync_storyline_projection` 中使用返回错误的 `let ... else`：

```rust
let ProjectionSourceSnapshot::CanonicalEvents {
    fact_version: previous_fact_version,
    fact_rows: previous_fact_rows,
    ..
} = &previous.source
else {
    anyhow::bail!("projection source is not canonical events; use `project rebuild`");
};
```

- [ ] **步骤 3：将其他三处不变量改为显式错误**

本地 manifest 对不受支持格式返回包含 format 与输入路径的 `anyhow` 错误；Catalog
发现对非 Events source 返回内部一致性错误；OpenAI corpus 恢复对未知 `group.kind`
返回包含 kind 与相对路径的 `Error::Other`。

- [ ] **步骤 4：运行严格 lint，确认 GREEN**

重复步骤 1 命令。

预期：通过。

- [ ] **步骤 5：运行行为测试并提交**

运行：

```bash
cargo test -p persisting-pchronicle --lib --locked
```

提交：

```bash
git add crates/persisting-pchronicle/src/projection/storyline.rs \
  crates/persisting-pchronicle/src/store/local_query_manifest.rs \
  crates/persisting-pchronicle/src/store/catalog/discovery.rs \
  crates/persisting-pchronicle/src/formats/openai_corpus.rs
git commit -m "refactor: replace pchronicle unreachable branches"
```

### 任务 3：收拢 Storyline 行模型

**文件：**
- 移动：`crates/persisting-pchronicle/src/storyline_schema.rs` → `crates/persisting-pchronicle/src/store/storyline/model.rs`
- 修改：`crates/persisting-pchronicle/src/store/storyline/mod.rs`
- 修改：`crates/persisting-pchronicle/src/store/storyline/rows.rs`
- 修改：`crates/persisting-pchronicle/src/store/mod.rs`
- 修改：`crates/persisting-pchronicle/src/lib.rs`

**接口：**
- 保留：crate 根的 `split_storyline`、`reconstruct_storyline`、四个行/表类型和三个表名常量。
- 删除：公开模块路径 `persisting_pchronicle::storyline_schema`。

- [ ] **步骤 1：运行现有 Storyline 模型测试，建立基线**

运行：

```bash
cargo test -p persisting-pchronicle storyline_schema::tests --locked
```

预期：全部通过。

- [ ] **步骤 2：移动逻辑模型并改为同目录引用**

将文件移动为 `store/storyline/model.rs`；在 `store/storyline/mod.rs` 声明
`mod model;`，并从本地 `model` 导入类型与函数。`rows.rs` 使用
`super::model::{StoryRunRow, StoryStepRow, StoryToolCallRow}`。

- [ ] **步骤 3：保持根级 item 导出并删除根模块**

由 `store/storyline/mod.rs` re-export：

```rust
pub use model::{
    reconstruct_storyline, split_storyline, StoryRunRow, StoryStepRow,
    StoryToolCallRow, StorylineTables, STORY_RUNS_TABLE, STORY_STEPS_TABLE,
    STORY_TOOL_CALLS_TABLE,
};
```

`store/mod.rs` 将这些 item 向上 re-export；`lib.rs` 从 `store` 导出它们并删除
`pub mod storyline_schema` 及对应导出块。

- [ ] **步骤 4：运行迁移后的测试与编译**

运行：

```bash
cargo test -p persisting-pchronicle --lib --locked
cargo check -p persisting-pchronicle-cli --tests --locked
```

预期：全部通过，且 `storyline_schema.rs` 不存在。

- [ ] **步骤 5：提交**

```bash
git add -u crates/persisting-pchronicle/src
git add crates/persisting-pchronicle/src/store/storyline/model.rs
git commit -m "refactor: colocate storyline row model"
```

### 任务 4：收拢 AgenticMD 领域实现

**文件：**
- 新建目录：`crates/persisting-pchronicle/src/agenticmd/`
- 移动：现有 AgenticMD codec、body、frontmatter、validation、mapping、layout、filesystem、conversion 与 projection 实现文件
- 修改：`formats/mod.rs`、`convert/mod.rs`、`layout/mod.rs`、`projection/mod.rs`、`store/mod.rs`、`lib.rs`
- 删除：根级 `mapping/` 模块与各旧 AgenticMD 实现文件

**接口：**
- 保留：当前 crate 根级 AgenticMD types、constants、codec、mapping、path、filesystem、conversion 与 projection item。
- 删除：`formats::agenticmd*` 等实现导向深层模块路径。

- [ ] **步骤 1：运行 AgenticMD 与 Gateway 行为测试，建立基线**

运行：

```bash
cargo test -p persisting-pchronicle agenticmd --locked
cargo test -p persisting-gateway --lib projection:: --locked
cargo test -p persisting-gateway --test agenticmd_bridge --locked
cargo test -p persisting-gateway --test agenticmd_golden --locked
cargo test -p persisting-gateway --test markdown_trajectory --locked
```

预期：全部通过。

- [ ] **步骤 2：创建私有领域模块并移动实现文件**

按规格创建：

```text
agenticmd/{codec,body,frontmatter,validate,layout,fs,convert,projection}.rs
agenticmd/mapping/{mod,fields,text}.rs
```

`agenticmd/mod.rs` 使用私有子模块，并以 `pub(crate) use` 或 `pub use` 聚合 crate
内部和根门面需要的 item；`projection` 保持 `#[cfg(feature = "lance-store")]`。

- [ ] **步骤 3：改写领域内部引用**

领域文件优先使用 `super` 或 `crate::agenticmd` 引用同域 item；Storyline、EventRecord
和 Store 类型继续从其所属模块导入。删除对旧 `crate::formats::agenticmd*`、
`crate::mapping` 和 `crate::layout::markdown` 的依赖。

- [ ] **步骤 4：重接公共门面**

`lib.rs` 增加私有 `mod agenticmd;`，删除 `pub mod mapping;`。根级 re-export
继续提供现有公共 item。`formats`、`convert`、`layout`、`projection` 和 `store`
只在其模块根直接 re-export 仍需保留的 item，不创建旧深层模块 wrapper。

- [ ] **步骤 5：运行 AgenticMD 与 Gateway 测试，确认行为不变**

重复步骤 1 的全部测试，并运行：

```bash
cargo check -p persisting-pchronicle --no-default-features --locked
cargo check -p persisting-pchronicle-cli --tests --locked
```

预期：全部通过；旧实现文件和根级 `mapping/` 不存在。

- [ ] **步骤 6：提交**

```bash
git add -u crates/persisting-pchronicle/src
git add crates/persisting-pchronicle/src/agenticmd
git commit -m "refactor: consolidate agenticmd domain"
```

### 任务 5：补齐 feature 与 panic 门禁

**文件：**
- 修改：`justfile`
- 修改：`.github/workflows/ci.yml`

**接口：**
- 输入：workspace strict Clippy 与四种 pChronicle 非 Search feature 组合。
- 输出：本地和 CI 共用的 `just lint-rust` 门禁。

- [ ] **步骤 1：确认当前门禁缺少 feature 矩阵**

运行：

```bash
just --dry-run lint-rust
```

预期：仅包含 workspace Clippy 和默认 feature pChronicle panic lint。

- [ ] **步骤 2：扩展 Just recipe**

使 `lint-rust` 依赖 workspace strict Clippy、默认 panic lint 和
`clippy-pchronicle-features`。所有 pChronicle library 命令统一使用：

```text
-D warnings -D clippy::unwrap_used -D clippy::expect_used -D clippy::unreachable
```

feature recipe 依次检查：

```text
--no-default-features
--no-default-features --features lance-store
--no-default-features --features oss-store
```

默认 S3 配置由现有 `clippy-pchronicle-panics` 覆盖。

- [ ] **步骤 3：让 CI 使用唯一入口**

将 `.github/workflows/ci.yml` 的 Rust clippy step 改为：

```yaml
- name: Rust clippy
  run: just lint-rust
```

- [ ] **步骤 4：运行门禁**

运行：

```bash
just --dry-run lint-rust
just lint-rust
```

预期：workspace 与四种 pChronicle feature 配置全部通过。

- [ ] **步骤 5：提交**

```bash
git add justfile .github/workflows/ci.yml
git commit -m "ci: check pchronicle feature matrix"
```

### 任务 6：最终验证与审查

**文件：** 不新增生产改动。

- [ ] **步骤 1：格式与残留扫描**

运行：

```bash
cargo fmt --all -- --check
cargo fmt --manifest-path pchronicle-web/Cargo.toml -- --check
rg -n '\b(unreachable!|panic!|todo!|unimplemented!)' \
  crates/persisting-pchronicle/src --glob '*.rs' --glob '!search/**'
```

预期：生产代码不再包含 `unreachable!`；测试中的 `panic!` 可保留。

- [ ] **步骤 2：核心与 CLI 测试**

运行：

```bash
cargo test -p persisting-pchronicle --lib --locked
cargo test -p persisting-pchronicle-cli --lib --tests --locked
cargo test --manifest-path pchronicle-web/Cargo.toml --locked
```

CLI loopback 测试如受 sandbox 限制，使用已授权的沙箱外 `cargo test` 重跑完整套件。

- [ ] **步骤 3：Gateway 回归**

运行：

```bash
cargo test -p persisting-gateway --lib projection:: --locked
cargo test -p persisting-gateway --test agenticmd_bridge --locked
cargo test -p persisting-gateway --test agenticmd_golden --locked
cargo test -p persisting-gateway --test markdown_trajectory --locked
```

- [ ] **步骤 4：最终门禁和 diff 审查**

运行：

```bash
just lint-rust
git diff --check
git status --short
```

确认只有用户原有未跟踪文件，所有计划内改动均已提交。
