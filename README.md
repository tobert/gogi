# gogi 合議

The council API contract that kaijutsu (client) and mkviolet (server) speak,
as documents and as code.

- `docs/` holds the contract. Start with `docs/council-api-v2.md`.
- `crates/gogi-proto` is the contract with no I/O.
- `crates/gogi-fake` is a fake server over a toy model.
- `crates/gogi-conformance` checks a live server:

```
cargo run -p gogi-conformance -- http://127.0.0.1:8091
cargo run -p gogi-conformance -- http://127.0.0.1:8091 --allow-restart 'systemctl --user restart mkviolet'
```

Each check prints `ok`, `skip`, or `FAIL`. The exit code is 1 when any check
fails. With `--allow-restart <cmd>` the suite also restarts the server and
checks that specs and pins survive.

```
cargo test --workspace
```
