# Storyline 统一 Unknown Fields 设计

## 背景

`StorylinePresence` 目前记录 ATIF 字段的 missing/null/value 三态、tool call
`extra` 的显式 `null`，以及输入文档的容器形状和顺序。这使 Storyline 可以恢复输入的
物理表示，但这些信息不是轨迹语义；它们还通过按层级硬编码的集合形成一套影子模型，新增
字段时必须同步扩展 presence 枚举和转换器。

ACTF 和 OpenAI Msg 又各自通过 `extra` 保存格式专属 residual。不同 codec 因而使用
不同的无损机制，格式专属数据会混入 Storyline 的业务 `extra`，跨格式多跳也无法统一携带。

本设计删除 `StorylinePresence`，改为统一、稀疏、受限的 unknown-fields residual。
这里的 “unknown” 指 **Storyline 没有正式字段承载的来源字段**，既包括来源格式规范中的
已知字段，也包括厂商扩展字段；它不表示来源 codec 一定不认识该字段。

## 目标

- Storyline 只把可理解的轨迹语义建模为正式字段。
- 所有双向外围格式使用同一套 unknown-fields 捕获、携带、恢复和校验机制。
- 同格式往返和跨格式多跳都保留 Storyline 未建模的 key 与 JSON value。
- 已知字段的 missing 与显式 `null` 等价，不再为此保存旁路状态。
- residual 保持稀疏，并有明确的条目数和字节数上限；超限时 fail closed。
- Lance 可以把 residual 中较大的重复 value 内容寻址到 `objects.lance`，但该优化不改变
  对外模型或自包含 wire 表示。
- 在轨迹级提供归一化 unknown-key 路径及出现次数，便于分析格式扩展的分布。

## 非目标

- 不保证空白、缩进、对象键顺序、数字原始词法或重复 JSON object key 的逐字节恢复。
- 不把 unknown value 解释成 Storyline 语义。
- 不保留输入是单对象、单元素数组、JSONL 还是 NDJSON；目标 codec 决定 canonical
  容器表示。
- 不从 Storyline 反建 Canonical Event。Canonical Event 到 Storyline 仍是单向投影。
- 本设计不进入 TTAS、Queue/Sampler、Search 或 `persisting-dlcapt`。

## 权威数据模型

`StorylinePresence`、`PresenceState`、字段枚举以及 `StorylineCollectionShape` 被删除。
`StorylineDocument` 改为持有格式隔离的 residual：

```rust
pub struct StorylineDocument {
    // existing canonical fields...

    #[serde(default, skip_serializing_if = "StorylineUnknownFields::is_empty")]
    pub unknown_fields: StorylineUnknownFields,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown_key_counts: BTreeMap<String, BTreeMap<String, u64>>,
}

pub struct StorylineUnknownFields {
    /// Canonical DocumentFormat name -> residual belonging to that source.
    pub sources: BTreeMap<String, SourceUnknownFields>,
}

pub struct SourceUnknownFields {
    /// Groups slices that came from the same physical source document.
    pub source_document_id: String,

    /// RFC 6901 JSON Pointer -> complete original JSON value.
    pub fields: BTreeMap<String, serde_json::Value>,
}
```

通常一条轨迹对每种来源格式只有一个 `SourceUnknownFields`。同一物理来源文档拆成多条
Storyline 时，例如 ACTF 的多个 attempt，文档级 unknown fields 逻辑复制到每条轨迹，
每条轨迹再保存自身子树的 residual。`source_document_id` 用来在导出时重新组合这些切片。
如果来源格式有稳定文档标识，codec 使用该标识；否则使用 canonical JSON 的 BLAKE3
摘要。计算摘要前先移除 `_storyline` envelope，并递归按 object key 排序。该 ID 只用于
residual 分组，不成为 Storyline 的轨迹身份。

`unknown_key_counts` 是物化的派生数据，不是恢复来源文档的事实源。它在导入、hydrate
和显式 Storyline 校验时从 `unknown_fields` 重新计算，不能单独修改。第一层 key 是
canonical `DocumentFormat` 名称，第二层 key 是把数组下标替换成 `*` 后的归一化
JSON Pointer。例如：

```json
{
  "atif": {
    "/steps/*/vendor_data": 12,
    "/agent/experimental_config": 1
  }
}
```

计数是轨迹局部统计。因为文档级 residual 会复制到同一来源文档产生的每条轨迹，聚合多条
轨迹的计数时也会看到相应重复；它不声称是物理源文件级的唯一出现次数。

## 捕获规则

每个外围 codec 声明其消耗到 Storyline canonical 字段的来源路径。导入时，codec 在原始
逻辑 JSON/YAML 树上执行 schema-aware 遍历：

1. 移除并解析保留的 Storyline envelope。
2. 把能映射的值写入 Storyline 正式字段。
3. 对每个没有正式 Storyline 承载位置的 object member，保存该 member 的精确 JSON
   Pointer 和完整 value。
4. 一旦一个 member 被判定为 unknown，就保存整个 value 子树，不再拆分叶子。
5. 对部分被消费的结构继续向下遍历，捕获其中未消费的直接 member。
6. 对 Storyline 以 `Value` 作为完整语义承载的开放字段，不检查其内部 key。

已知字段的显式 `null` 和 missing 都不进入 residual。unknown key 即使 value 是 `null`
也必须进入 residual，因为此时 key 的存在本身属于需要保留的数据。空数组、空对象、`false`
和数值零同样不得省略。

精确路径使用 RFC 6901 JSON Pointer，不实现完整 JSONPath。codec 在捕获路径时知道每个
segment 的父容器类型，因此可以可靠地把数组下标归一化为统计路径中的 `*`；数字形式的
object key 不会被误判成数组下标。

## 统一 Wire Envelope

JSON 外围格式使用保留的 `_storyline` object。一个物理文档只含一条轨迹时，仍使用统一
的 `by_trajectory` 形状，避免单条和多条出现两套语义。`by_trajectory` 的 key 是目标
codec 生成的 carrier JSON Pointer：它指向目标物理文档中承载该轨迹的对象，而不是假设
跨格式转换前后的 Storyline identity 必然相同。空 pointer 表示物理文档根对象。

```json
{
  "_storyline": {
    "unknown_fields": {
      "version": 1,
      "by_trajectory": {
        "/attempts/1": {
          "sources": {
            "atif": {
              "source_document_id": "source-id",
              "fields": {
                "/steps/0/vendor_data": {"x": 1}
              }
            }
          }
        }
      }
    }
  }
}
```

一个物理目标文档承载多条 Storyline 时，目标 codec 必须为每条轨迹生成唯一 carrier，
例如 ACTF attempt 的 `/attempts/1`。重新导入时，codec 通过 carrier 把 residual 分发给
对应轨迹，不要求跨格式转换前后的 Storyline identity 相同。重复、失效或指向非 object
的 carrier 都是输入错误。

AgenticMD 使用相同逻辑 envelope，但放入现有 Storyline frontmatter metadata，而不是在
Markdown 正文中发明新的 block。Storyline Lance 直接持久化权威字段，不使用 wire
envelope。Canonical Event 不携带 envelope。

`_storyline` 是保留扩展字段：解析时它不会再次被捕获成 unknown。`unknown_fields.version`
当前只接受整数 `1`；非 object、缺少或未知版本、
无法关联的 `by_trajectory` 或格式不合法的 envelope 都 fail closed。

## 跨格式展开与携带

目标格式采用“展开自身，携带其他来源”的规则：

- 导出格式 `F` 时，把 `sources[F]` 恢复到格式 `F` 的原始位置，并从 envelope 中移除
  该来源，避免原生字段和 envelope 重复。
- 其他来源 residual 原样放进目标格式的 `_storyline` envelope。
- 重新导入目标格式时，原生 unknown fields 被重新捕获，携带的其他来源被解包回对应
  Storyline。

例如 ATIF residual 经过 ACTF 时作为 ACTF envelope 中的不透明数据携带；再次导出 ATIF
时恢复到 ATIF 路径。ACTF 消费者无需理解 ATIF 字段，但数据不会丢失。

同一来源文档拆出的多条 Storyline 在导出时按 `source_document_id` 合并。各轨迹复制的
文档级路径必须值相同；相同值去重，不同值报冲突。各轨迹子树路径取并集。

## 恢复和冲突规则

codec 先从 Storyline canonical 字段生成目标文档，再恢复 residual：

- residual 只能写入目标 codec 当前仍不映射到 Storyline 正式字段的路径。
- Storyline canonical 字段永远是权威值，residual 不得覆盖它。
- residual 路径在新版 codec 中变成正式字段时，如果 canonical 值与 residual 值不同，
  导出失败；不静默覆盖或丢弃。
- 输入同时在原生位置和 `_storyline` 中携带同来源、同路径数据时，值相同则去重，值不同
  则输入无效。
- 不同来源格式位于不同 namespace，相同 JSON Pointer 不构成冲突。
- 恢复前验证 JSON Pointer，并验证所有父容器存在且类型正确。
- Storyline 修改导致数组元素删除、重新排序或父路径失效时，恢复失败，不根据邻近元素
  猜测新位置。

所有冲突错误都包含来源格式、`source_document_id`、轨迹 identity 和 JSON Pointer。

## 容器和顺序

物理容器由目标 codec 的 canonical policy 决定：要求数组的格式始终输出数组，要求单
document 的格式输出对象，JSONL/NDJSON 由文件编码层逐行输出。单对象和单元素数组被视为
等价，不参与无损比较。

Storyline 自身的稳定轨迹顺序继续由 `Vec<StorylineDocument>` 和 Lance
`storage_ordinal` 保证。原输入的 `collection_shape` 和 `collection_ordinal` 不再进入
Storyline 模型。

## 大小限制

限制按每条 Storyline、所有来源 residual 合计，在任何 offload 之前计算：

- 最多 4096 个 unknown field；
- 所有 exact pointer、`source_document_id` 和 value 的紧凑 JSON 表示合计最多 1 MiB；
- 两个限制都必须是有限正数，可以通过 codec/import options 调高或调低；
- 任一限制超出时拒绝整条输入，不截断、不丢弃、不只保留计数；
- `_storyline` envelope 的结构开销和派生的 `unknown_key_counts` 不计入 value 字节数；
- count 使用饱和加法，避免恶意输入触发整数溢出。

超限错误报告实际条目数、实际字节数、配置上限以及按序列化大小排序的最大若干路径。
字节数的确定算法是：每个来源的 `source_document_id` UTF-8 长度计算一次，再加每个
pointer 的 UTF-8 长度和 `serde_json::to_vec(value)` 的长度；map/envelope 标点不计入。

## Lance 存储与 `objects.lance`

`runs` 投影新增 `unknown_fields_json` 和 `unknown_key_counts_json`。旧的
`presence_json` 不再写入；读取旧数据集时忽略其中的 missing/null 和容器形状信息，并在
新字段缺失时返回空 residual。向旧 schema 追加前必须先增加新的 nullable columns；旧
数据无法追溯生成 unknown fields。

ACTF/OpenAI Msg 现有的 `persisting.dev/...` 格式 residual 不再写入 Storyline 业务
`extra`。升级旧数据集时，迁移器用旧 codec 重建对应外围文档，再由新 codec 捕获为精确
pointer map；重建或分组不唯一时迁移失败，不静默把旧 residual 当成普通业务 `extra`。
纯业务 `extra` 和无法识别为既有格式扩展的内容保持不变。

多 attempt 等场景仍在每条轨迹逻辑复制文档级 residual，以保持读取和删除单条轨迹时的
模型自足。为了控制物理重复，Lance content externalizer 在 residual 的 value 边界工作：

- 小 value 内联在 `unknown_fields_json`；
- 达到现有 content offload threshold 的 value 按紧凑 JSON bytes 计算 BLAKE3 content
  ID，写入 `objects.lance`，run row 保存内部 descriptor；
- 多条轨迹复制的相同 value 因 content ID 相同只存一份 object；
- public reader 在返回 `StorylineDocument` 前完整 hydrate descriptor；
- wire envelope 永远写回完整 JSON value，不暴露内部引用；
- admission limit 按 hydrate 后的逻辑数据计算，不能靠 offload 绕过；
- 缺失 object、长度不符或 hash/codec 冲突都 fail closed。

如果用户提供的 string value 与内部 descriptor magic 前缀冲突，externalizer 必须像现有
content cell 逻辑一样强制 offload/escape，保证用户值不会被误当成引用。

## 无损定义

同格式或跨格式回程后的来源文档满足以下条件时，称为 JSON 数据模型级语义无损：

- Storyline 已知字段经过 codec canonicalization 后相等；
- 已知字段的 missing 与显式 `null` 等价；
- unknown key 仍存在，且对应 `serde_json::Value` 相等；
- unknown value 为 `null` 时也必须保留该 key；
- object key 顺序不参与比较；
- array 顺序和元素参与比较；
- empty、false 和 zero 不被当成 missing；
- 目标格式的 canonical 容器形状不与输入物理形状比较；
- `_storyline` 只作为传输 envelope，不算来源格式业务字段。

数字比较使用 `serde_json::Value` 的数据模型相等，不保存原始数字词法。重复 JSON object
key 在解析阶段已不属于可表达的数据模型，因此不在保证范围内。

## Codec 边界

格式专属逻辑保持在对应 codec 中：

```rust
trait UnknownFieldCodec {
    fn capture_unknown_fields(
        &self,
        input: &serde_json::Value,
        stories: &mut [StorylineDocument],
    ) -> InputResult<()>;

    fn restore_unknown_fields(
        &self,
        stories: &[StorylineDocument],
        output: &mut serde_json::Value,
    ) -> Result<()>;
}
```

通用 residual 层负责 JSON Pointer、envelope、namespace、限额、计数、合并和通用冲突
检查；codec 负责 consumed-path schema、动态 map/array 的合法结构、轨迹 identity 对应和
格式 canonical container。这样新增格式时复用安全策略，但不把格式 schema 硬编码回
Storyline 核心。

## 验证与测试

至少覆盖以下测试组：

1. ATIF、ACTF、OpenAI Msg 和 AgenticMD 的同格式往返。
2. 任意 JSON 外围格式 `A -> Storyline -> B -> Storyline -> A` 的跨格式往返。
3. 上述路径经过 Storyline Lance 三表和 `objects.lance` 后的往返。
4. root、嵌套 object、动态 map、数组元素、空值和 `null` unknown value。
5. 已知字段 missing/null 等价，而 unknown null key 必须保留。
6. 多 ACTF attempt 和多 OpenAI record 的共享 residual 复制、合并、去重与冲突。
7. 同来源原生值/envelope 值相同去重、不同值报错。
8. malformed pointer、父容器类型错误、数组改序和路径失效。
9. 4096/1 MiB 边界恰好通过，任一超过一单位时整条拒绝。
10. 大 residual value offload、跨轨迹 content-ID 去重、hydrate 和 object 缺失失败。
11. legacy `presence_json` 读取为空 residual，以及新 nullable columns 的 schema 升级。
12. unknown-key wildcard 统计、数字 object key、饱和计数和跨来源 namespace 隔离。

验收不包含工作区默认排除的 TTAS、Queue/Sampler、Search 和 `persisting-dlcapt` 测试。

## 文档变化

pChronicle 文档中的无损边界改为：known missing/null 被 canonicalize；所有 Storyline 未
建模的来源字段通过统一 unknown-fields residual 保留；跨格式多跳通过 namespaced
`_storyline` envelope 携带。删除“ATIF 三态”和“输入单对象/数组形态属于 Storyline
集合语义”的现有描述，并说明 4096/1 MiB fail-closed 限制与 `objects.lance` 仅是内部
物理优化。
