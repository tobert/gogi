# gogi-fake: the toy model

`gogi-fake` serves council v2 over a model small enough to read in one sitting. Run it
with `cargo run -p gogi-fake -- --listen 127.0.0.1:8091 [--state-dir DIR] [--model NAME]...`,
or from Python with `with gogi.FakeServer(state_dir=DIR) as fake:`, which serves at `fake.url`.

## Served models

By default the fake serves one model, `gogi-toy-1`, with the alias `toy`, so a request may
leave out `model`. Each `--model NAME` serves a model of that name, ready; with two or more, a
request names one. Every served model uses the template below. Each has a seed, its position
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

A token is one of the 256 byte values, or one of 13 template markers (ids 256 to 268).
Text becomes its UTF-8 bytes, so content never spells a marker. Only a `reads` suffix does:
marker text in a suffix, such as `<user>`, becomes one marker id, and the reply lists it in
`signals.control_text`.

## Template

```text
<bos> [<nothink>] [<tools> TOOLS_JSON <endtools>] MESSAGE*

system     <sys> content <end>
user       <user> content <end>
assistant  <asst> [<think> reasoning <endthink>] content (<call> name(ARGS_JSON) <endcall>)* <end>
tool       <tool> content <end>

generation prompt   <asst> [<think>]
```

- `<nothink>` renders when the context sets `thinking: false`. With thinking on, every
  assistant message renders its `<think>` region, empty when it has no reasoning.
- A call's `id` is never rendered.
- The thinking rule: an assistant message without tool calls loses its `reasoning` once a later
  user message follows. A message with tool calls keeps it.
- A marked message is a boundary only when its own rendering is a prefix of the whole
  context's. The context's end is always one. So an assistant message with reasoning and no tool
  calls, marked `snap`, stops being a boundary when a user message is added after it.

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
  first tool with `{}`. Call ids are `call_{prompt tokens}_{k}`.

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

## Limits

`context_tokens` defaults to 4096, `decision_context_tokens` to 3072, `pin_bytes` to 1 MiB,
and `max_snapshots` to 10,000. Each is a field of `Config` in Rust and an argument of
`gogi.FakeServer` in Python. A prompt past
`context_tokens` is a `413` with `error.type` `context_full`.
