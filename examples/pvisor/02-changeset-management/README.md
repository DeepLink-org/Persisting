# 1.2 pVisor 的 changeset 管理

问题：同一个 staged changeset 能否先查看，再明确选择应用或删除？

脚本创建两个独立 Run：第一个通过 `review --json` 查看后 `apply`，第二个查看后
`drop`。最终直接检查 base 文件系统。

```bash
./run.sh
```

预期：review 共看到 3 个变化；2 个被应用，1 个被删除且没有进入 base。
