# 2. 内置分析与轨迹定位

这个示例直接分析 [`examples/data`](../../data/) 下的 ATIF、ACTF 和 OpenAI Messages
三个确定性 Dataset，不需要先转换格式或启动服务。

它展示四个稳定的内置分析入口：

- `analysis overview`：汇总 Sources、Trajectories、Steps、Agents、Models 和工具调用；
- `analysis agents`：按 Agent 身份聚合活动；
- `analysis models`：汇总声明和实际观测到的模型使用；
- `analysis tools`：按规范化函数名聚合工具调用。

最后，脚本使用 `find --session-id ... --step-id ...` 定位一条具体 Step，并验证所有输出：

```bash
./run.sh
```

示例只读取仓库内 fixture，不写入 Warehouse 或用户设置；完整命令日志保存在本场景的
`.work/run.*`。
