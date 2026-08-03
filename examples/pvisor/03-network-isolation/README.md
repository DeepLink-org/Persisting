# 1.3 pVisor 的网络系统隔离

问题：pVisor 的 OverlayNet policy 是否会控制它截获到的 HTTP 请求？

脚本启动一个本地 HTTP server。一个 Run 显式 allow 该地址，另一个 Run 显式 deny；
两个客户端都强制经过注入的 HTTP proxy。最后读取两个 Run Bundle 的 policy counters。

```bash
./run.sh
```

预期：`policy_allowed=1`、`policy_denied=1`。Bundle 同时报告
`network_non_bypassable=false`，因为直接 socket 不在这个 cooperative proxy 实验内。
