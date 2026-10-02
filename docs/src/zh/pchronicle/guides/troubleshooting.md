# 排查一个 Dataset

每次排查 pChronicle 结果时都按相同顺序进行：先确认路径，再查看可见内容，最后缩小查询。
这样可以区分 Dataset 缺失、结果为空和资源上限，而不是把它们看成同一个错误。

## 先确认 Dataset

排查时先使用具体路径。pin 会增加一次解析步骤：

```bash
pchronicle dataset list
pchronicle stats ./trajectory-data --format json
pchronicle list ./trajectory-data --format json
```

如果 pin 失败，先解析 pin，再排查存储凭据或 SQL：

```bash
pchronicle dataset show prod
pchronicle stats @prod --format json
```

Pin 只指向 Dataset，不会复制或移动底层数据。

## Dataset 能打开但看起来为空

在编写更复杂的过滤器前先看汇总：

```bash
pchronicle stats overview ./trajectory-data
pchronicle find ./trajectory-data --match "" --format json
```

空结果可能表示路径包含受支持格式但没有匹配记录，也可能是过滤器作用在错误的实体上，或
Dataset 中的文件不是 pChronicle 可以识别的格式。overview 和 JSON metadata 会说明可见
Source 与搜索模式。

## 查询没有返回行

先执行有上限的 count，再确认规范化表名：

```bash
pchronicle query ./trajectory-data \
  --sql 'SELECT COUNT(*) AS runs FROM dataset.runs'
pchronicle query ./trajectory-data \
  --sql 'SELECT source, COUNT(*) AS steps FROM dataset.steps GROUP BY source'
```

先用 `find` 定位 identity 或文本，再编写 join。Snapshot 会固定一次读取视图；如果两次命令
之间数据发生变化，请记录 JSON 输出中的 Snapshot identifier，并在后续查询中复用。

## 查询触发资源上限

资源上限是公共查询契约的一部分。先缩小问题，再提高限制：

```bash
pchronicle query ./trajectory-data \
  --sql 'SELECT source, COUNT(*) FROM dataset.steps GROUP BY source' \
  --max-output-rows 20 --timeout 10s
```

CI 中使用 `--file` 和明确的输出限制。需要更大预算时，应在调用工作流中说明原因，而不是
静默移除保护。

## Source 格式不受支持

先查看[支持的格式](../reference/formats/index.md)，再使用 exchange 指南导入为 pChronicle
可以规范化的 Dataset。导入不会补造缺失的 lineage 或 Evidence；需要追溯时，请保留原始
Source 与规范化视图。

## 确认本地块缓存是否生效

远端 Lance 读取使用按对象版本区分的持久化磁盘缓存，默认块大小为 1 MiB。
范围读取和整文件读取复用同一套块；同一进程重新打开 Dataset 会复用元数据并合并并发下载。
命中会更新持久化访问时间，worker 重启后仍能优先保留近期使用的块。
占用达到容量的 90% 后，后台淘汰以 80% 为目标，为后续下载预留空间。

| 环境变量 | 默认值 | 用途 |
| --- | --- | --- |
| `PCHRONICLE_LANCE_CACHE_DIR` | 系统缓存目录下的 `pchronicle/blocks` | 共享缓存目录，catalog worker 也遵循此设置 |
| `PCHRONICLE_LANCE_CACHE_CAPACITY_BYTES` | `536870912`（512 MiB） | 磁盘缓存容量目标 |
| `PCHRONICLE_LANCE_CACHE_BLOCK_SIZE_BYTES` | `1048576`（1 MiB） | 对齐下载与缓存的块大小 |

打开 debug 日志，重复执行同一条远端查询：

```bash
pchronicle --log-level debug query @prod \
  --sql 'SELECT COUNT(*) FROM dataset.steps'
```

查看 `pchronicle.block_cache` 日志：`block cache miss` 表示下载块，
`block cache hit` 表示从磁盘命中；`block cache status` 给出命中、未命中、
命中/下载字节、合并读取、淘汰、写入失败次数，以及最近一次磁盘扫描统计的占用。
`CachedObjectStore::stats()` 提供同样的指标；同一进程内相同缓存配置共享计数。
另一个 CLI 进程会复用磁盘块，仍会查询远端元数据。

元数据最多复用 60 秒。显式 HEAD 和 Lance 的可变版本提示仍查询远端；
写入、分片上传完成、复制或删除成功后，会使相关元数据失效。
本地缓存写入失败会记录日志，已下载的数据仍正常返回；大于总容量的块直接返回而不缓存。

实现参考了 [Doris 文件缓存](https://doris.apache.org/docs/3.x/compute-storage-decoupled/file-cache/file-cache-internals/)
的对齐分块和持久化 LRU 思路。淘汰定期扫描磁盘，也统计其他 worker 写入的文件。
下载合并作用于同一进程；不同进程仍可能并发下载同一个冷块。
尚未实现优先级队列与预热。

## 提交 issue 前

请提供 pChronicle 版本、Dataset 路径或 pin 名称（不要包含凭据）、`status --format json` 输出、
完整查询和资源限制。对象存储还应说明 Provider 类型及 region 或 endpoint，但不要提供 access key
或签名 URL。
