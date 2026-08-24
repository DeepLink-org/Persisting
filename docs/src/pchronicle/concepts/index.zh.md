# Dataset

**Dataset 是 pChronicle 面向用户的唯一数据对象。** 它是一组可以被浏览、查询、分析、导入、
导出或提供服务的 Agent 运行数据。

一个 Dataset 可以表现为：

- 本地目录或文件（`./local/path`）；
- 对象存储中的 URI 前缀（`s3://bucket/prefix`）；
- 指向上述位置的用户 alias（`@alias-name`）。

## Dataset 的写法

裸字符串按路径或 URI 解释；`@` 前缀明确表示 alias：

```text
prod       本地相对路径 ./prod
@prod      名为 prod 的 Dataset alias
```

因此，同名目录和 alias 不会让命令产生歧义。使用 `pchronicle alias` 创建和查看 alias；使用
`pchronicle default` 选择省略参数时使用的本地 Dataset。

## 命令看到什么

pChronicle 会发现 Dataset 中受支持的 Run 数据，并把语义兼容的字段规范化为 `runs`、`steps`
和 `tool_calls` 等查询表。每条读取命令使用一个内部一致的视图；即使底层位置在命令执行期间
发生变化，已经开始的读取也不会随之漂移。

这就是使用命令行所需的完整用户模型。存储发现、版本固定、事实、projection 和 revision 属于
实现与数据契约细节；只有集成确实依赖这些边界时，才需要继续阅读[设计](../design/index.md)。

接下来可以进入[常见工作流](../guides/index.md)、[产品术语](../reference/terminology.zh.md)或
[命令行参考](../reference/cli.md)。
