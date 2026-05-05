# `persisting trajectory` 命令设计说明

`persisting trajectory` 面向 **Agent 执行轨迹** 场景，提供轨迹记录的追加、回放、统计三类操作。

轨迹的命名空间由 `STORAGE`（根目录）与 `NAME`（逻辑名）二元组定位。

---

## 1. 命令树

```
persisting
└── trajectory
    ├── add     # 追加轨迹记录
    ├── replay  # 分页回放
    └── stats   # 统计信息
```

---

## 2. 全局选项

根命令 `persisting` 的 `--core-lib` 对所有子命令生效，详见 [CLI 整体架构设计](cli_architecture.zh.md)。

---

## 3. 子命令

### 3.1 `persisting trajectory add`

向指定命名空间追加一批轨迹记录。

```
persisting trajectory add <STORAGE> <NAME> [OPTIONS]
```

#### 参数

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `STORAGE` | `String` | 是 | — | 轨迹根目录（位置参数） |
| `NAME` | `String` | 是 | — | 轨迹逻辑名（位置参数） |
| `--format` | `jsonl` \| `toml` \| `ronl` | 否 | `jsonl` | 输入格式 |
| `--input` | `String` | 否 | `-` | 输入文件路径，`-` 表示 stdin |

#### 输入格式

**`jsonl`**（默认）：每行一个 JSON 值，空行跳过。适合管道与 agent 日志导出。

**`toml`**：单个 TOML 文档，根表须含 `records` 键且为数组。支持 `[[records]]` 数组表和 `records = [...]` 内联数组两种写法。适合手写配置与评审场景。

**`ronl`**：每行一个 RON 值，空行跳过。输入原样传递，不做格式转换。适合直接构造 wire 格式的场景。

---

### 3.2 `persisting trajectory replay`

按分页参数回放轨迹记录。

```
persisting trajectory replay <STORAGE> <NAME> [OPTIONS]
```

#### 参数

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `STORAGE` | `String` | 是 | — | 轨迹根目录（位置参数） |
| `NAME` | `String` | 是 | — | 轨迹逻辑名（位置参数） |
| `--trajectory-id` | `String` | 否 | — | 会话 / run id（预留） |
| `--offset` | `usize` | 否 | `0` | 起始偏移 |
| `--limit` | `usize` | 否 | — | 最大返回条数；省略由引擎默认 |

---

### 3.3 `persisting trajectory stats`

查询命名空间的统计与路径信息。

```
persisting trajectory stats <STORAGE> <NAME>
```

#### 参数

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `STORAGE` | `String` | 是 | 轨迹根目录（位置参数） |
| `NAME` | `String` | 是 | 轨迹逻辑名（位置参数） |

---

## 4. 设计约定

- **资源锚点前置**：与 `search` 一致，`STORAGE` + `NAME` 为前两个位置参数。
- **CLI 负责格式归一**：jsonl / toml 在客户端转为统一格式，减少后端复杂度。
- **ronl 为透传模式**：`--format ronl` 下输入原样传递，不做转换。
- **默认 stdin**：`--input` 默认 `-`，支持管道。与 `search create` 不同：**stdin 时 `trajectory add` 仍可使用 `--format` 的默认值 `jsonl`**；若 stdin 实为 **ronl / toml**，须显式传 **`--format`**。
- **输出到 stdout**：成功时 pretty 打印结果，失败时打印错误信息并返回非零退出码。
