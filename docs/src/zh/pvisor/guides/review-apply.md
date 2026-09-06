# 审查并应用 Agent 修改

Agent 可以在 staged workspace 内自由工作，但不会自动获得修改基础项目的权限。Run
结束后，由你决定哪些 Effect 可以跨过这条边界。这个工作流应用文件系统
[Effect 模型](../concepts/run-model.md)。

:::note 开始前
你需要一个项目目录和一个可以运行 Agent 的命令。如果还没有完成第一次运行，请先完成[运行第一个 Agent](../get-started.md)。
下面的流程假设你希望手动决定 staged 修改何时进入基础项目。
:::

## 使用 manual stage 运行

最短命令是：

```bash
pvisor -- codex
```

也可以显式指定路径和提交方式：

```bash
pvisor run \
  --overlayfs-compose "$PWD" \
  --stage /tmp/my-agent-stage \
  --overlayfs-commit manual \
  -- codex
```

每个并发 Run 必须使用独立 stage。

## 接受前先审查

```bash
pvisor review last
pvisor inspect last -- git status --short
```

`review` 汇总 Run Bundle、文件 Effect、网络证据与安全警告。`inspect` 在 staged view 中
执行只读检查命令。

## 只应用选中的文件

可以按路径或 pattern 选择：

```bash
pvisor apply last --path src
pvisor apply last --include 'tests/**' --exclude 'tests/generated/**'
```

过滤后的 apply 只消费选中的、依赖闭合的变更批次。Opaque directory 与 hard-link group
不能在会产生无效结果时被强行拆分。

## 多次 apply

应用一部分修改不会关闭 stage。可以重新审查剩余内容，再应用下一批：

```bash
pvisor review last
pvisor apply last --path docs
pvisor review last
pvisor apply last --all
```

成功批次记录在 `apply-ledger.json` 中。你可以随时停止，并丢弃尚未应用的修改：

```bash
pvisor drop last
```

:::tip 验证结果
每次批量 apply 后，都要对比 staged view 与基础项目。`apply` 成功只表示选中的、依赖闭包完整的批次已经跨过边界，
不表示所有剩余 Effect 都已应用。
:::

## 从已接受状态继续

开始下一条工作线之前创建 checkpoint：

```bash
pvisor checkpoint last --name accepted-base
pvisor fork last --checkpoint accepted-base -- codex
```

新 Run 拥有独立 identity 与 stage，同时保留到该 checkpoint 的 lineage。

## 这条边界不覆盖什么

选择性 apply 只治理 staged filesystem Effect。它不能撤销消息、支付、部署、直接数据库
写入或外部 API mutation。这些 Effect 需要执行前 admission、Provider 支持时的 containment，
以及执行后的持久 evidence。

下一步：[控制网络访问](network.md)或[捕获 Run](capture.md)。
