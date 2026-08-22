# Storyline `/prompt`：ACTF `system_prompt` / `user_content`

## Status

Implemented. Approach C approved in conversation on 2026-08-22: keep `/turns/{t}/msg`
as the assistant utterance; store the ACTF prompt pair as `{system, user}` JSON
on the document and, when it differs, on the turn.

This increment supersedes the residual classification of those two ACTF step
keys in [2026-08-22-storyline-task-env-response-design.md](2026-08-22-storyline-task-env-response-design.md).
Attempt `extra` / `meta` stay residual.

## Context

ACTF 一步同时带三份文本：`system_prompt`、`user_content`、
`assistant_content.content`。当前只有助手正文进入 `/turns/{t}/msg`；两份 prompt
落入 `unknown_fields`，导出时被写成空字符串。

`/task/env` 是运行时/基础设施，不是这两份模型输入。`msg` 已是助手正文的权威槽，
不能改成 `{system, user}` 或 `{system, user, content}`。

实测语料上同一 attempt 的 5454 步往往共享同一对 prompt。权威槽必须能去重，
否则每条 `message_json` 都会再存一份。

## Goals

1. ACTF `system_prompt` 与 `user_content` 各有恰好一个权威目标；导入后不再进入
   `unknown_fields`。
2. `/turns/{t}/msg` 继续只映射 `assistant_content.content`；字符串 `msg` 保持合法。
3. 文档基线 + turn 整段覆盖；覆盖相对 `/prompt`，不相对上一 turn。
4. `schema_version` 仍为 `storyline/v1`。Lance 仍是三表，不新增第四张表。
5. OpenAI / ATIF / AgenticMD / Events 不新增一等 prompt 字段。

## Non-goals

- 把 `src` 与 `msg` 收成 enum，或改变 `msg` 的话语语义。
- 把 prompt 写进 `/task`、`/task/env`、`turns[].env`、`extra` 或 `unknown_fields`。
- 拆出 `src=system` / `src=user` turn，或改变 `step_id` / turn 基数。
- 把 OpenAI / ATIF 的 system/user 消息提升到 `/prompt`。
- 提升 attempt `extra` / `meta`。
- 新增 `storyline/v2`。
- 改变 TTAS、Queue、Search、`persisting-dlcapt`。

## Wire

`prompt` 保持全名（与 `task` / `env` 相同）。对象 `deny_unknown_fields`。

```text
/prompt
├── system     string?   ACTF system_prompt
└── user       string?   ACTF user_content
```

根对象增加可选 `/prompt`。`turns[]` 增加可选 `/prompt`，形状相同。

空对象、只含空字符串的对象，在**文档**上视为缺省，不序列化。
`system` / `user` 若为空字符串，默认不序列化。

`copied == true` 的 context turn 不得写 `prompt`。

`effective_kind()` 不读取 `prompt`。

## 有效 prompt（整段覆盖，不是浅合并）

```text
effective(turn) =
    turn.prompt          若该 turn 有 /prompt
    否则 document /prompt
```

turn 一旦带 `/prompt`，就整段替换文档基线：缺省的 `system` / `user` 视为空字符串，
**不**从文档继承。不合并更早的 turns。查询层按此计算；存储层不物化合并结果。

因此，只要某步与文档基线不同，导入必须把**当前完整 pair**写到该 turn
（即使只有一侧变了，未变的一侧也要写上，否则整段覆盖会把它打成空）。

两侧都空、且文档基线非空时，turn 必须显式写出
`{"system":"","user":""}`，以便和「缺省 = 继承文档」区分。这种显式空 pair
是 turn 上唯一允许的「双空」`/prompt`。

## ACTF 映射（RFC-0004）

`pair(step) := (system_prompt, user_content)`，空字符串就是空字符串，不是缺失。

| ACTF JSON Pointer | Storyline JSON Pointer |
|---|---|
| `/attempts/{a}/trajectory/steps/{s}/system_prompt` | 见下：文档 `/prompt/system` 或 `/turns/{t}/prompt/system` |
| `/attempts/{a}/trajectory/steps/{s}/user_content` | 见下：文档 `/prompt/user` 或 `/turns/{t}/prompt/user` |
| `/attempts/{a}/trajectory/steps/{s}/assistant_content/content` | `/turns/{t}/msg`（不变） |

每个源 pointer 仍只有一个权威目标。同一对值不会同时写在文档和该 turn 上。

导入算法（单次顺序扫描即可，但空步若出现在基线之前，需要在基线确定后补写）：

1. `baseline` = 第一个至少一侧非空的 `pair`。写入文档 `/prompt`（空字符串键省略）。
   若全部 step 都是双空，文档 `/prompt` 缺省。
2. 对每个 step，按 turn 顺序：
   - `pair == baseline`（双空对「无基线」也算相等）→ 已消费，turn 不写 `/prompt`。
   - 否则 → 该 turn `/prompt` = 当前完整 pair。双空且文档有基线时写
     `{"system":"","user":""}`。
3. 基线落在 step `k`、且 `0..k-1` 为双空时：那些 turn 必须带显式空
   `/prompt`，否则还原会错误继承后来的基线。

导出：

```text
system_prompt = effective(turn).system or ""
user_content  = effective(turn).user or ""
```

ACTF 这两个键保持必填字符串；缺省写成 `""`，不再无条件空写。

`system_prompt` / `user_content` 不再列入残差表。attempt `extra` / `meta` 仍是残差。

## 其它格式

- OpenAI Messages：不写 `/prompt`。角色已经是独立 turn 的 `src` + `msg`。
- ATIF / AgenticMD / Events：不扩张一等 schema。Storyline-only `/prompt` 走既有
  `_storyline` envelope，不进 ATIF `extra`。
- 没有 `/prompt` 的普通 Storyline 导出 ACTF 时，两键仍为 `""`（合成转换，与现在一致）。

## Lance

不新增表。JSON 列保存完整对象，不把 `system` / `user` 展平为独立列。

| 表 | 新列 | 内容 |
|---|---|---|
| `runs` | `prompt_json` | 文档 `/prompt`，或缺省 |
| `steps` | `prompt_json` | 该 turn 的 `/prompt`，或缺省 |

旧表缺列按缺失解码。`content.rs` 的 `externalize_batch` 对缺列 skip。

## 校验

- `/prompt` 若存在：文档侧至少一个键为非空字符串。
- turn 侧：至少一个非空键，或两个键都在且都是空字符串（显式清空）。
- 空对象 `{}` 非法（文档与 turn 皆是）。
- `system` / `user` 必须是字符串；其它 JSON 类型导入失败。
- 未知键 `deny_unknown_fields`。

## 实现落点

- `crates/persisting-pchronicle/src/formats/storyline.rs`：`StorylinePrompt`，文档与 turn 字段
- `crates/persisting-pchronicle/src/convert/actf.rs`：导入算法与导出还原
- `crates/persisting-pchronicle/src/store/storyline/{model,rows,content}.rs`：`prompt_json`
- Gateway / CLI 测试里的 `StorylineTurn` / `StorylineDocument` 字面量补字段
- RFC-0001 wire 表；RFC-0004 权威映射与残差表；`docs/src/pchronicle/design/storyline-lance.md`

## 验收

1. 原 ACTF 语料导入 `storyline-lance` 后，
   `/attempts/*/trajectory/steps/*/system_prompt` 与
   `/attempts/*/trajectory/steps/*/user_content` 不再出现在 unknown-field warning。
2. 全程同一 pair：只出现文档 `/prompt`，turns 不写 `/prompt`；`msg` 仍是各步助手正文。
3. 中途 pair 变化：变化步带完整 turn `/prompt`；未变步不写；导出还原每步原字符串。
4. 前缀双空、其后出现非空基线：前缀 turn 带显式空 `/prompt`，导出仍是 `""`。
5. ACTF → Storyline → ACTF 还原这两键，不再依赖 `unknown_fields`。
6. 既有 ATIF / OpenAI fixture 与无 `/prompt` 的旧 Storyline 仍合法。
7. 仍为残差：attempt `extra`、`meta`。
