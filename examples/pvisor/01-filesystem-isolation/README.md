# 1.1 pVisor 的文件系统隔离

问题：Agent 修改文件时，原始项目目录会不会立即改变？

脚本把 `base/` 作为 OverlayFS base，让 Agent 修改一个文件并新增一个文件。结束后
直接比较 base、stage upper 和 Run Bundle。该实验测量事务工作区隔离，不声称 Host 进程
无法访问其他宿主路径。

```bash
./run.sh
```

预期：base 保持原值，upper 和 Bundle 都记录 2 个变化，
`filesystem_non_bypassable=false`。
