# gogi 合議

The council API contract, as documents and as code.

A **council** asks a model the same questions about one **case** after each
of several **contexts**, then pools the answers. Each context is a chat, and
each gives its own read of the case. The response reports a probability for
every option, for each context and pooled, so a client sees both the answer
and where the contexts disagree.

One use is a gate in front of a coding agent. Before the agent runs a shell
statement, kaijutsu sends the statement as the case. Each context holds one
source of guidance, and the **spec** holds the questions, such as `verdict`
with the options `allow`, `ask`, and `report`. The server never decides; the client
applies its own thresholds.

The client sends each context whole with every request. The server holds
**snapshots** of model state (the KV cache) at the **boundaries** the client
marks, and starts each request from the longest snapshot it holds, so a
request runs only the tokens after it. The same snapshots serve
**generations**, so one server can run a client's chats and its council.

`docs/council-api-v2.md`, "Terms", defines each word in bold.

These projects speak the contract:

- **[kaijutsu](https://github.com/tobert/kaijutsu)** is the client. It is an
  agent environment whose gate and chats call a council server.
- **mkviolet** is a server. It runs Gemma 4 and serves council v2. It is
  private, so its source paths cited in `docs/` cannot be followed from here.
- **lfm2d** is a server with its own decision routes (`/v1/opinion`). An
  update will bring it to council v2 and make it the public server
  implementation.

gogi holds the contract itself: the documents, code that checks requests and
responses against them without doing I/O, and a test suite to run against any
server.
Where kaijutsu and mkviolet disagree, `docs/disagreements.md` records it.

## Examples

`examples/walkthrough.sh` is one council session in curl, a request at a
time: a chat whose held snapshots make each new message cheap, a fork, a pin,
a spec, and a decision pooled over two contexts of guidance.

```
cargo run -p gogi-fake -- --listen 127.0.0.1:8091 &
examples/walkthrough.sh
```

`examples/council_tour.py` covers the same ground in Python and adds a queue
context that receives the case as a tool result, a restart, and expiring pins. It
checks each server number with gogi's own code. It starts `gogi.FakeServer` in
its own process, or takes a server URL.

```
cd crates/gogi-py && maturin develop && cd ../..
python examples/council_tour.py
```

## Layout

- `docs/` holds the contract. Start with `docs/council-api-v2.md`.
- `crates/gogi-proto` is the contract with no I/O.
- `crates/gogi-fake` is a fake server over a toy model.
- `crates/gogi-py` binds the validation, the math, and the fake server to
  Python (`cd crates/gogi-py && maturin develop && pytest tests`).
- `examples/` walks through a council session: `walkthrough.sh` in curl, and
  `council_tour.py` in Python.
- `crates/gogi-conformance` checks a live server:

```
cargo run -p gogi-fake -- --listen 127.0.0.1:8091 &
cargo run -p gogi-conformance -- http://127.0.0.1:8091
cargo run -p gogi-conformance -- http://127.0.0.1:8091 --allow-restart 'systemctl --user restart my-council-server'
```

Each check prints `ok`, `skip`, or `FAIL`. The exit code is 1 when any check
fails. With `--allow-restart <cmd>` the suite also restarts the server and
checks that specs and pins survive.

```
cargo test --workspace
```

## License

gogi is licensed under either of the [MIT license](LICENSE-MIT) or the
[Apache License, Version 2.0](LICENSE-APACHE), at your option.
