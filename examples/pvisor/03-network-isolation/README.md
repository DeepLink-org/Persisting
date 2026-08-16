# 1.3 pVisor 网络边界

这个例子只回答一个问题：pVisor 当前控制的是哪些网络流量？

```bash
./run.sh
./test.sh  # 执行同一场景并验证预期结果
```

脚本启动一个本地 HTTP server，然后平铺执行三个场景：

1. `--overlaynet-allow` 允许经过代理访问声明的目标；
2. `--overlaynet-deny-all` 拒绝经过代理的请求；
3. `curl --noproxy "*"` 直接连接同一目标，证明 direct socket 可以绕过代理。

核心命令就是普通的 pVisor CLI：

```bash
pvisor run --overlaynet-allow 127.0.0.1:<ephemeral-port> -- \
  agent-command

pvisor run --overlaynet-deny-all -- \
  agent-command
```

纯 OverlayNet 运行会将当前目录作为 Run 的项目关联路径；每次执行的 Run 记录和
Bundle 则独立保存在 `PERSISTING_RUN_HOME` 下。

`run.sh` 中的短 `bash -c` 只负责让 curl 显式读取 pVisor 注入的 `$HTTP_PROXY`。
它把 allow、deny 和 direct 三次执行的 stdout、stderr 与退出码保存在工作目录中，便于
直接观察。`test.sh` 再检查响应、预期失败和三个 Run Bundle。

预期结论：

```text
Conclusion: OverlayNet controls cooperative proxy traffic; it is not a network sandbox.
```

## 安全边界

当前 OverlayNet 是显式 HTTP/HTTPS proxy，不是透明 network namespace：

- 遵守 `HTTP_PROXY` / `HTTPS_PROXY` 的客户端会经过策略；
- 删除代理设置、配置 `NO_PROXY` 或直接创建 socket 可以绕过；
- DNS/UDP 不在当前驱动覆盖范围内；
- Run Bundle 的 `network_non_bypassable` 因此为 `false`。

需要不可绕过的网络隔离时，应使用 container/KVM 网络边界，而不是把 cooperative proxy
的 allowlist 或 deny-all 当作安全沙箱。
