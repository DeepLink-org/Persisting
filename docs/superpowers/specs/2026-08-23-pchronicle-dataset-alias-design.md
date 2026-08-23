# pChronicle Dataset 路径快捷描述符

日期：2026-08-23
状态：待审
范围：`persisting-pchronicle-cli` 的 Dataset URI / `--from` 路径解析
非目标：库 API 改签名、用户自定义 alias 配置、新 URI scheme、`--from-codex`、把 vendor 目录设成默认 Warehouse、SQL 字符串内展开、监听或写入 `~/.codex` / `~/.claude`

## 1. 决策

在 CLI 路径槽增加内置描述符 `@codex` 与 `@claude`。它们在进入现有探测 / 查询 / import 之前展开成真实目录。描述符就是路径，不是第二种数据源。

```bash
pchronicle query @codex 'SELECT COUNT(*) AS runs FROM dataset.runs'
pchronicle query @codex/2026/05/29 'SELECT COUNT(*) AS runs FROM dataset.runs'
pchronicle import --from @claude --output ./claude-ds
pchronicle query --dataset vendor=@codex 'SELECT COUNT(*) AS runs FROM vendor.runs'
```

`ls` / `status` / `analysis` 的 Dataset 位置参数同样展开。不新增 `--from-codex`。

锁定方案 **1**（CLI 单点展开）：

- 展开发生在 `normalize_and_validate_dataset_uri` 之前（或作为其第一步）
- `import --from` 在任何文件系统访问之前走同一展开函数（今日 import 并不都经过 canonicalize）
- 库 `ChronicleQueryEngine::open` 等仍只收真实路径
- 未知 `@名字` fail closed，不当成本地文件名

## 2. 语法

输入必须**整段**以 `@` 开头才视为描述符。`./@codex`、`foo/@codex`、`s3://bucket/@codex` 不展开。

| 输入 | 含义 |
|---|---|
| `@codex` | Codex 会话根目录 |
| `@claude` | Claude Code 项目根目录 |
| `@claude-code` | `@claude` 的同义词 |
| `@codex/2026/05/29` | 根目录 + 相对后缀 |
| `@codex/` | 与 `@codex` 相同 |
| `@unknown` | 未知名字 → 错误 |
| `-` | 仍是 stdin；不是描述符 |

名字匹配大小写敏感，只认上述三个 token。分隔符只认 `/`（含 Windows）。后缀去掉全部前导 `/` 后按 `Path::new(root).join(remainder)` 拼接，因此 `@codex//etc` 落在会话根下的 `etc`，不会跳到文件系统根。

拒绝：

- `@` 后面没有名字
- 后缀拆出的分量含 `..` 或为空（`@codex/../.ssh`、`@codex/foo/../bar`）
- `@codex://...` 这类伪 scheme

未知描述符错误形如：`unknown dataset alias '@foo'; expected @codex or @claude`。

## 3. 展开

| 描述符 | 根目录 |
|---|---|
| `@codex` | `${CODEX_HOME:-$HOME/.codex}/sessions` |
| `@claude` / `@claude-code` | `${CLAUDE_CONFIG_DIR:-$HOME/.claude}/projects` |

`CODEX_HOME` / `CLAUDE_CONFIG_DIR` 为空字符串时视为未设置。相对环境变量值先相对当前工作目录变成绝对路径，再拼 `sessions` / `projects`。

找不到 home（且对应环境变量未设）时：`cannot resolve @codex: home directory is unknown`。

展开结果是绝对路径字符串，再交给现有 `normalize_and_validate_dataset_uri`（本地路径会 canonicalize）。目录不存在时不在 alias 层发明新错误，沿用 canonicalize / 打开数据源的现有失败。

对 vendor 目录只读。展开本身不创建目录。

## 4. 作用面

调用展开的 CLI 槽：

- `query` / `ls` / `status` / `analysis` 的 Dataset 位置参数
- `query --dataset NAME=URI` 的 URI 一侧
- `import --from`（文件或目录；`--from -` 除外）
- `export --from`（展开后若不是可导出 Dataset，走现有错误）
- `serve --storage` 的 URI 一侧（含 `NAME=URI`）
- `resolve_dataset_uri` 与 Warehouse 配置里的 URI（同一函数，避免第二套解析）

不展开：

- SQL 文本
- `--output` / `--settings` 等非 Dataset 路径
- Agent 注入用的 `CODEX_HOME/skills`（那是另一条代码路径）

## 5. 错误与测试

单元测试（不碰真实 `~/.codex`）：

- `@codex` / `@claude` / `@claude-code` 在临时 `HOME`（或注入的 `CODEX_HOME` / `CLAUDE_CONFIG_DIR`）下展开到预期根路径
- `@codex/2026/05/29` 拼接正确
- `@unknown`、`@codex/../x`、`./@codex` 行为符合上表
- `--from -` 不被当成 alias

CLI 测试：在临时目录放下一份 Codex/Claude JSONL，把 `CODEX_HOME` 或 `CLAUDE_CONFIG_DIR` 指过去，然后 `query @codex 'SELECT COUNT(*) AS runs FROM dataset.runs'` 得到 1。

`--help` 不必枚举全部 alias；文档（exchange / cli）写一行示例即可。

## 6. 非目标再确认

不把 `@codex` 设成省略 Dataset 参数时的默认 Warehouse。用户仍可显式 `pchronicle query @codex '...'`。不在本次做可配置 alias 表。
