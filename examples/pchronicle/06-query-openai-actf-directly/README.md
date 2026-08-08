# 2.6 直接查询 OpenAI JSON 与 ACTF 目录

这个示例使用 `ppilot query sql` 直接分析 OpenAI-message JSON 和 ACTF 文件目录，不需要
先创建 Lance store。pChronicle 会把输入临时规范化为 `runs`、`steps`、`tool_calls`，并在
三张查询表上增加 `_file_` 列：

- 单文件输入时，`_file_` 是文件名；
- 目录输入时，`_file_` 是相对输入目录、使用 `/` 分隔的路径；
- SQL 可以用 `LIKE` 的 `%` 和 `_` 通配符按路径筛选；
- `_file_` 只存在于直接文件查询视图，不写入 Lance 三表。

运行：

```bash
./run.sh
```

脚本会完成四项检查：

1. 自动识别带嵌套目录的 OpenAI JSON corpus，并用 `LIKE 'batch/%'` 只打开指定路径；
   目录中故意放置了一个未命中的损坏 JSON，用它验证文件级裁剪确实发生在读取之前；
2. 显式使用 `--source actf` 查询 ACTF 目录，并用文件名通配符筛选；
3. 验证直接文件查询的 `DESCRIBE runs` 包含 `_file_`；
4. 把其中一个 OpenAI 文件转换为 Lance 后，验证 Lance 的 `runs` schema 不含 `_file_`。

示例使用仓库内裁剪、脱敏 fixture，所有生成内容保存在 `.work/`。目录自动识别会冻结
递归发现的文件 manifest，并从稳定排序后的第一份文件推断格式。可下推的 `_file_ =`、
`IN` 和 `LIKE` 条件会先裁剪 manifest，只有命中文件才会被读取和规范化；实际扫描到的
混合格式或损坏 JSON 会明确报错。

同一查询的三张表共享有界解析缓存；多文件 join 必须把 `_file_` 和 `session_id` 一起作为
join key，例如：

```sql
SELECT s.session_id, t.function_name
FROM steps s JOIN tool_calls t
  ON s._file_ = t._file_
 AND s.session_id = t.session_id
 AND s.step_id = t.step_id
```

生产任务可以用 `--max-files`、`--max-entries`、`--max-file-bytes`、`--max-concurrent-files`、
`--cache-bytes`、`--cache-files` 和 `--batch-size` 设置资源边界；`--query-metrics` 把运行
计数器写到 stderr，`--memory-limit-bytes`、`--spill-path`、`--max-spill-bytes` 限制
DataFusion 中间算子，`--timeout-seconds` 限制墙钟时间。查询结果按 Arrow batch 流式写出；
`--max-output-rows` 限制结果规模。直接 JSON 查询仍按单文件完整解析，超大、重复查询的
数据集应先转换为 Lance。

核心查询与普通 SQL 相同：

```bash
ppilot query sql ./openai-data \
  --sql "SELECT _file_, COUNT(*) AS steps
         FROM steps
         WHERE _file_ LIKE 'batch/%'
         GROUP BY _file_"
```
