# 2. 内置分析与轨迹定位

**问题：三种交换格式能否不经转换就跑通内置分析并定位指定 Step？可复现结论：overview 汇总 3 个 ready Source / 4 条轨迹 / 9 个 Step；`find` 定位 `support-001` step 1。**

这个示例直接分析 [`examples/data/corpus`](../../data/corpus/) 下的扁平多格式
Dataset（ATIF、ACTF、OpenAI Messages），不需要先转换格式或启动服务。

它展示四个稳定的内置分析入口：

- `stats overview`：汇总 Sources、Trajectories、Steps、Agents、Models 和工具调用；
- `stats agents`：按 Agent 身份聚合活动；
- `stats models`：汇总声明和实际观测到的模型使用；
- `stats tools`：按规范化函数名聚合工具调用。

最后，脚本使用 `find --session-id ... --step-id ...` 定位一条具体 Step，并验证所有输出。

## Run

```bash
./run.sh
```

示例只读取仓库内 fixture，不写入 Warehouse 或用户设置；完整命令日志保存在本场景的
`.work/run.*`。

## Links

- [pChronicle examples](../README.md)
- [Discover and query](../../../docs/src/pchronicle/guides/discover-and-query.zh.md)
