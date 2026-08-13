# pChronicle server tests

Server tests are split by ownership:

- module-local unit tests cover query validation, acceleration, aggregation,
  asset handling, and endpoint implementation details;
- `http_contract.rs` exercises the public Router as a black box and fixes the
  Warehouse boundary: documented reads work, mutation routes stay absent,
  evidence SQL is read-only and bounded, and unknown API paths never receive
  the SPA shell.

Run the server gate with:

```bash
cargo test -p persisting-pchronicle-server --lib --test http_contract
```
