# Analyze 提问 / 写 SQL 双入口设计

日期：2026-08-23
状态：待审
范围：`pchronicle-web` Analyze 工作区输入卡与主按钮。不改编译器、catalog 契约、EXPLAIN、有界执行。
修订：`2026-08-23-pchronicle-analysis-spec-compiler-design.md` 第 1 节「SQL 不是功能入口、手写是逃逸舱藏在高级详情」的 **界面呈现**。编译器与手写跳过 Spec 修复循环的语义保持不变。

## 1. 决策

**输入用 tab 二选一：提问，或写 SQL。执行轨迹和结果共用。SQL 始终是将要执行的那一份。**

- **提问**：模型写 Spec，编译器出 SQL，写入同一份 SQL，再 EXPLAIN → 执行 → 解读。
- **写 SQL**：编辑同一份 SQL。改字即手写，跳过 Spec 与修复循环，仍走 EXPLAIN → 执行 → 解读。
- Analyze 再次提问会按当前问题重编 Spec，并覆盖这份 SQL。

## 2. 页面结构

左侧 Catalog 不变。右侧主栏从上到下：

1. 页标题「Ask a question. Or write SQL.」与工具条（最近分析、设置）。
2. **一张输入卡**，tab：`提问` | `写 SQL`。
3. **How Analyze ran**（现 02）：两条路径共用轨迹表。
4. 结果卡、解读、修订时间线：共用，现有组件。
5. Spec 摘要：仅提问路径且尚未手写时显示。

**删除**独立的「03 Compiled SQL」卡。SQL 编辑器只活在「写 SQL」tab。

输入卡不再套外层容器；tab 面板就是卡的内容区。

## 3. 输入卡

### 3.1 提问 tab

- 问题 textarea、starter chips、上下文芯片（catalog / 只读 / scope）。
- 主按钮 **Analyze**。
- 运行中按钮显示当前阶段（Writing spec / Compiling SQL / Executing / Interpreting）。
- Spec 编译失败的错误挂在这张卡上。

### 3.2 写 SQL tab

- 同一 `revision.plan.sql` 的 textarea。无独立草稿。
- 占位：`SELECT …`
- 手写后显示「Manually edited」。
- 主按钮 **Run**。
- Catalog 点表或字段：插入当前 SQL 光标。若焦点在提问 tab，先切到写 SQL 再插入。
- 运行中或生成中锁定编辑器（与现 `sql_locked` 相同）。

### 3.3 主按钮

由 **当前 tab** 决定，不是由「有没有 SQL」决定。

| 当前 tab | 按钮 | 行为 |
|---|---|---|
| 提问 | Analyze | `generate_plan`：Spec → 编译（覆盖 SQL）→ 执行 → 解读 |
| 写 SQL | Run | 现有手写路径：有 SQL 则 EXPLAIN + 执行 + 解读，不修 Spec |

提问 tab 在问题与已审问题不一致、且未手写时：Analyze 按 **当前问题** 重跑（覆盖 SQL）。不要同时保留「为旧问题跑 SQL」的歧义按钮。

空 SQL 时 Run 禁用。空问题时 Analyze 禁用。

## 4. 共用轨迹与结果

`AnalyzeTraceView` 仍是 02。

| 路径 | 轨迹步骤 |
|---|---|
| 提问 | Write spec → Compile SQL →（失败则 Repair spec）→ Execute → Interpret |
| 写 SQL | Execute → Interpret（无 Write spec / Compile） |

结果表、列画像、解读、follow-up 与修订时间线不随 tab 切换而卸载。切 tab 只换输入卡内容。

手写路径不展示 Spec 摘要。提问成功且未手写时展示。

## 5. 状态

沿用 `AnalysisRevision.manually_edited` 与现 `apply_manual_sql`：

- 编辑 SQL → `manually_edited = true`，清 evidence / interpretation，进入可 Run 状态。
- Analyze 成功编译 → `manually_edited = false`，SQL 为编译产物。
- 不新增第二份 SQL 字段。

## 6. 文案

禁止：只读编译产物、escape-hatch、高级详情里的 SQL。

- 提问 tab 说明：Analyze compiles a spec, then runs bounded evidence.
- 写 SQL tab 说明：Run executes this query. Editing skips spec repair.
- 手写徽章：Manually edited。

## 7. 非目标

- 不改 AnalysisSpec 编译器、schema 白名单、EXPLAIN、行上限。
- 不让模型直接写 SQL。
- 不把 Data / Runs / 详情页改成这套 tab。
- 不做 SQL 与问题的双向同步（不从 SQL 反推问题）。

## 8. 验收

- 提问 tab Analyze 后，写 SQL tab 里是编译出的 SQL；02 与结果在下方，切 tab 不消失。
- 改 SQL 后徽章为 Manually edited；Run 不再走 Spec 修复；02 从 Execute 起。
- 提问 tab 再 Analyze，SQL 被新编译结果覆盖，徽章消失。
- Catalog 插字段落在 SQL 编辑器；在提问 tab 点击会切到写 SQL。
- 不再出现第三张 Compiled SQL 卡。
- `pchronicle-web` 测试覆盖 tab 与 `manually_edited` 切换；嵌入 UI 须重新打包 serve 后验收。
