# 1.1 pVisor 的文件系统隔离

问题：Agent 修改文件时，原始项目目录会不会立即改变？

`run.sh` 把 `base/` 作为 OverlayFS base，让 Agent 修改一个文件并新增一个文件，然后
展示 base、stage upper 和 Run Bundle。`test.sh` 对这些产物执行回归断言。该实验测量
事务工作区隔离，不声称 Host 进程
无法访问其他宿主路径。

```bash
./run.sh
./test.sh  # 执行同一场景并验证预期结果
```

预期：base 保持原值，upper 和 Bundle 都记录 2 个变化，
`filesystem_non_bypassable=false`。
