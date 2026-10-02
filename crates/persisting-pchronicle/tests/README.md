# pChronicle 测试布局

格式编解码、路径与错误行为由模块单元测试覆盖。`tests/*.rs` 验证 Storyline
存储往返、文件查询、格式 corpus、公共接口和真实 S3 契约。存储并发与发布保证
由 `src/store/storyline/tests.rs` 覆盖。

```bash
just test pchronicle
just proptest pchronicle
just benchmark-pchronicle
```

性质测试位于 `tests/proptests/` 和私有模块的 `proptests`，由 `proptest` feature
启用。默认功能测试不运行它们。真实 S3 测试默认忽略，需要隔离的
`PCHRONICLE_S3_TEST_URI` 后执行 `just test-pchronicle-s3`。
