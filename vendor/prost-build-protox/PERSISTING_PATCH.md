# Persisting patch

This directory vendors `prost-build` 0.14.3 from the upstream Prost project.
The only functional change is in `Config::load_fds`: protobuf source is parsed
by pure-Rust `protox` 0.9.1 instead of spawning `protoc`.

Prost still performs Rust code generation, so configuration such as
`extern_path`, `bytes`, and `enable_type_names` retains upstream behavior. The
adapter accepts Lance 9.0.1's
`--experimental_allow_proto3_optional` argument because protox implements that
syntax without an opt-in flag; other protoc-only arguments fail explicitly.

When updating Prost or Lance, compare this directory with the matching
`prost-build` release and rerun the generated-output hash comparison for all
six Lance build-script consumers.
