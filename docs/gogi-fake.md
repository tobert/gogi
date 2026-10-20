# gogi-fake: the toy model

`gogi-fake` serves the council API over a model small enough to read in one sitting. Run it
with `cargo run -p gogi-fake -- --listen 127.0.0.1:8091 [--state-dir DIR] [--model NAME]...`,
or from Python with `with gogi.FakeServer(state_dir=DIR) as fake:`, which serves at `fake.url`.

The toy model lives in the `gogi-toy` crate, which does no I/O and has no HTTP: the tokens and
markers, the tokenizer, the template with its boundaries and the thinking rule, the generation
prompt, the model state and logits, and a reply parser. `gogi-fake` serves it over HTTP. Another
engine can use `gogi-toy` alone to serve the same model.

## Served models

By default the fake serves one model, `gogi-toy-1`, with the alias `toy`. Every request that
reads a context names its served model in `model`, as the contract requires. Each `--model NAME`
serves a model of that name, ready. Every served model uses the template below, whose id is
`gogi-toy-chat-2`. Each has a seed, its position
among the `--model` flags, that changes its numbers and its `weight_hash`, so two served models
give two sets of numbers and two sets of snapshots.

In Rust, `Config::models` lists `FakeModel`s with any status, and `Server::set_status` changes
one while the server runs, as a server does when it loads or unloads a model. Python has the
same: `gogi.FakeServer(models=["day", {"name": "night", "aliases": ["n"], "seed": 7, "status":
"unloaded"}])` and `fake.set_status("night", "ready")`. A model given as a name is ready, with
no aliases and its position as its seed. Two served models with the same seed give the same
numbers but hold separate snapshots. A restart returns each model to its configured status. A request to a
served model that is not `ready` is a `503` with `error.type` `model_unavailable`. Snapshots
and pins stay held through the change.

## Tokens

A token is one of the 256 byte values, or one of 14 template markers (ids 256 to 269).
Text becomes its UTF-8 bytes, so content never spells a marker. Only a `reads` suffix does:
marker text in a suffix, such as `<user>`, becomes one marker id, and the reply lists it in
`signals.control_text`.

## Template

```text
<bos> [<nothink>] [<tools> TOOLS_JSON <endtools>] MESSAGE*

system     <sys> content <end>
user       <user> content <end>
assistant  <asst> [<think> reasoning <endthink>] content (<call> name(INPUT_JSON) <endcall>)* <end>
tool       <tool> [<error>] content <end>

generation prompt   <asst> [<think>]
```

- `<nothink>` renders when the context sets `thinking: false`. With thinking on, every
  assistant message renders its `<think>` region, empty when it has no reasoning.
- `TOOLS_JSON` is the context's `tools` as JSON, each `{name, description, input_schema}`.
- A call's `id` is never rendered. `<error>` renders when a tool message has `is_error: true`.
- The thinking rule: an assistant message without tool calls loses its `reasoning` once a later
  user message follows. A message with tool calls keeps it.
- A marked message is a boundary only when its own rendering is a prefix of the whole
  context's. The context's end is always one. So an assistant message with reasoning and no tool
  calls, marked `snap`, stops being a boundary when a user message is added after it.

## Reply parser

`gogi_toy::reply::ReplyParser` reads a reply's token ids, one at a time, after the generation
prompt. `ReplyParser::new(thinking)` takes `true` when the prompt ended with `<think>`. For each
token, `push` returns its phase (`reasoning`, `content`, or `call`) and the text it adds. A marker
in reasoning or content adds no text, and a byte that does not yet complete a character adds none.
`<endthink>` ends the reasoning and belongs to it. `<call>`, `<endcall>`, and the tokens between
them belong to `call`.

`finish` returns the reply: `content`, `reasoning`, the calls, and `ended`, which is true when
the reply wrote `<end>`. A call `<call> name(INPUT_JSON) <endcall>` parses to its name and its
input, a JSON object with its members in the order written. A call that is not `name(INPUT_JSON)`,
or has no `<endcall>` before the reply stops, is kept as written with an error. The parser makes
no call ids; the server does. A token after `<end>`, or an id of 270 or more, panics.

## Model

The model's state after a token prefix is a hash folded over the served model's seed and the
tokens. The logits are a
function of that hash, between -6 and 6, so the same prefix gives the same bits.

- Raw reads and decisions read the logits of the letters `A`, `B`, ... after the context and
  the suffix or the question.
- A generation samples among the letters `a` to `z`, the space, and the end token. With
  thinking on it first writes its thought, ending at `<endthink>`. At temperature 0 it takes the
  largest logit. Without `sample.seed` the seed comes from the prompt, so a reply replays.
- A context with tools whose last message contains the text `CALL` gets a reply that calls its
  first tool with the input `{}`. Call ids are `call_{prompt tokens}_{k}`.
- `finish` is `end_turn` or `tool_use` when the reply wrote `<end>`, `max_tokens` when it wrote
  `max_tokens` tokens, and `context_full` when the prompt and the reply reached
  `context_tokens`. `usage.reasoning` counts the tokens of the thought, `<endthink>` included.
- A context whose last message contains the text `FAIL` gets no reply: without `stream`, a `500`
  with `error.type` `internal`; with `stream`, two `token` events and then an `error` event,
  so a client can test how it handles a failure after the stream began.
- A stream sends one `ping` before its first `token` event, so a client can test that it
  ignores it.

## Held state

- Snapshot ids hash the served model, the tokens, and the boundary counts along them. A
  build starts only from the snapshots of the served model it names.
- A build starts from the longest held snapshot whose tokens begin the context, and reports
  `kept`, `fed`, and `from`. A generation holds the head after its reply, so the next request
  starts there even though the reply is not marked `snap`.
- Pins keep a snapshot and its parents. `pin_bytes` caps the bytes they keep together.
  `max_snapshots` drops the oldest unpinned snapshots first.
- With `--state-dir`, specs, pins, and the snapshots pins keep survive a restart. Other
  snapshots do not.

## Requests and errors

- Every response carries a `request-id` header, `req_<16 hex digits>_<n>`, and every error body
  and `error` event repeats it as `request_id`. A `traceparent` header is accepted and ignored.
- A body that does not decode is a `400`. When a member is missing or unknown, `error.param`
  names it, such as `model` or `state`.

## Limits

`context_tokens` defaults to 4096, `decision_context_tokens` to 3072, `pin_bytes` to 1 MiB,
and `max_snapshots` to 10,000. Each is a field of `Config` in Rust and an argument of
`gogi.FakeServer` in Python. A prompt past
`context_tokens` is a `413` with `error.type` `context_full`.
