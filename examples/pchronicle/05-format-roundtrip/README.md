# 2.5 使用 pPilot 导入并恢复外围格式

这个示例只调用产品命令 `ppilot convert`，演示两条完整路径：

```text
OpenAI corpus JSON → Storyline Lance 三表 → OpenAI corpus JSON
ACTF JSON          → Storyline Lance 三表 → ACTF JSON
```

输入使用 pChronicle 测试中的裁剪、脱敏 fixture。脚本在恢复后使用 `jq` 比较 JSON 数据
模型，验证键值、显式 `null`、嵌套结构、数组顺序和源文件分组保持一致；不比较空白、缩进
或对象键顺序。

```bash
./run.sh
```

通过 `just examples-pchronicle` 运行时，仓库会先构建 release 版 `ppilot`。直接运行脚本
时，如果 `target/release/ppilot` 不存在，脚本也会自动构建。生成的 Lance store 和恢复
文件保留在 `.work/` 中，便于继续检查。
