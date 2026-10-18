# Council API v2 (draft)

Council v2 is a small HTTP contract for a model server that holds snapshots
of model state. The client sends a whole context with every request; the
server holds a snapshot at each boundary the client marks and starts each
request from the longest snapshot it holds. Three requests start from
snapshots: decisions, raw reads, and generations. A server that implements
the contract serves kaijutsu's chats and its council from one store, so the
two share one KV cache and one eviction policy. One server serves one or
more models, and each request names the model it reads.

This draft replaces `docs/council-api.md` (v1) and the three servers' own
context routes: the HIP megakernel's `/mk/v1`, lfm2d's `/v1/opinion`, and
mkviolet, which serves both. "Converting" below says what each one changes.
mkviolet serves this draft (its `src/council_v2.rs` and `src/held.rs`), and
kaijutsu's mk provider and council speak it. `gogi-conformance` checks a
server against it.

## What changes from v1

- **The server keeps no context records.** A request carries its context
  whole, and the server holds only snapshots, which are named by their
  tokens, and specs, which are named by their content. Neither can disagree
  with the client: a client that sends a context gets that context, and a
  restart, a second client, or a lost record costs feeding, never a wrong
  answer. v1's context ids, `If-Match`, and "PUT it again" are gone.
- **Contexts are native chat.** A context is `messages` in the model's own
  chat shape, tool calls and tool results included, not v1's system and
  `turns`. The council bench measured why (mkviolet's
  `web/council.html`): Gemma 4 keeps its own thinking in view only inside
  one agent turn. Queue contexts built that way put more than 0.9999 of the
  probability on the answer letters on every case tried, where v1's lettered
  menu put a median of 0.94 on `verdict`.
- **One store for three requests.** Decisions (typed and pooled), raw reads
  (letter log probabilities), and generations (sampled replies) all start
  from the same held snapshots. v1 had only decisions; generation lived in each
  server's private routes.
- **The case can arrive as a tool result.** A queue context receives the
  case as the result of its last call, and the model answers by calling the
  queue tool again.
- **Pins have owners.** A client pins the snapshots it needs under its own
  name, for a time it renews.
- **A server serves several models.** Identity lists each served model with
  its own identity, limits, and capabilities, and a status that changes when
  the server loads or unloads it. Every request names its served model with
  `model`. v1's server had one model.
- **Decisions are the council's own.** v1 accepted any request in the
  gateway Decisions API's shape. v2 does not: a decision names a held spec,
  sends its case as `case`, and has no inline questions and no gateway
  fields.
- **Tools and calls take Anthropic's shape.** A tool is `{name, description,
  input_schema}` and a call is `{id, name, input}`.
- **Every response has a request id,** and every response counts tokens in
  one `usage` shape.

## Terms

This table is the source for the contract's words. Each term names one
thing, and each thing has one term.

| Term | Meaning |
|---|---|
| council | Several contexts that read the same case with the same spec, each on its own, with their reads pooled into one set of answers. |
| context | A chat the client sends whole with each request: `messages`, `tools`, `thinking`, and `queue`. It is a value, not a record: the client keeps it, and the server never stores it. |
| message | One entry of a context's `messages`, with the role `system`, `user`, `assistant`, or `tool`. |
| agent turn | The assistant messages and tool messages between one user message and the next. |
| queue tool | The tool a context's `queue` names. |
| queue context | A context whose last message is an assistant call to its queue tool. A decision gives it the case as that call's result. |
| marked message | A message with `snap: true`. It ends at a boundary unless the rendering drops part of it. |
| boundary | The end of a marked message, when the rendering up to it is a prefix of the whole context's rendering. The end of a context is always a boundary. |
| snapshot | Model state, the KV cache, that the server holds at one boundary for one served model. Its id names that model state and no other, so a snapshot never changes. |
| head | The snapshot at the end of a context. |
| built on | A snapshot is built on the snapshots at earlier boundaries of the same tokens; the nearest one is its parent. |
| build | To render a context, start from the longest held snapshot whose tokens begin it, feed the remaining tokens, and hold a snapshot at each boundary. `kept` counts the tokens reused and `fed` the tokens run. |
| hold | What a server does with snapshots and specs, the only things it keeps between requests. |
| tier | Where a snapshot's model state is stored: `device`, `host`, or `disk`. |
| pin | An owner's claim that keeps a snapshot, and the snapshots it is built on, from eviction until `ttl_s` runs out. |
| owner | The name a client pins under. |
| spec | A held set of questions, with instructions and an input label. Its id is the sha256 of its canonical JSON. |
| question | One item of a spec: `choice`, `score`, `noul`, or `text`. |
| option | One possible answer to a `choice` question, or one level of a `score` question. |
| letter | The token `A`, `B`, and so on, that stands for an option when the model answers. |
| case | What a decision judges, sent as the request's `case` field. |
| decision | A request that reads one case after each of its contexts and pools the reads. |
| read | The numbers one context gives for one input. A decision read answers the spec's questions about the case; a raw read, from `POST /reads`, gives letter log probabilities after a client's suffix. |
| answer | One question's result, either from one read or pooled over all of a decision's reads. |
| pool | The answers combined over a decision's reads, `linear` or `loglinear`. |
| generation | A request that samples a reply after a context. |
| prompt | The rendered context a generation starts from, with the template's start of the reply. `usage.prompt` and `context_full` count it. |
| riding decision | A spec's questions asked during a generation, `before` or `after` the reply. Capability `ride`. |
| reply | The assistant message a generation writes. |
| response | The HTTP body a server returns for any request. |
| request id | The id a server gives one request, sent in the `request-id` header of its response and in every error body. |
| trace context | The client's W3C `traceparent` header, which joins the server's spans to the client's trace. It travels only as a header. |
| usage | The token counts a response reports: `prompt`, `kept`, `fed`, `completion`, and `reasoning`. |
| phase | The part of a reply a streamed token belongs to: `reasoning`, `content`, or `call`. |
| identity | The model, weights, tokenizer, template, and engine that produced a number. Each served model has one. |
| served model | One identity a server serves, with its own limits, capabilities, and status. A request names it with `model`. |
| status | Whether a served model takes requests now: `ready`, `loading`, `draining`, or `unloaded`. |
| template | How a server renders messages, specs, and cases into tokens. It is part of identity. |
| capability | An optional part of the contract a server declares in identity, such as `reads` or `pins`: for the server, or for one served model. A client ignores a capability it does not know. |
| class | How a request is scheduled: `interactive` or `batch`. It never changes a number. |

## Endpoints

| Method and path | Does |
|---|---|
| `GET /council/v2/identity` | The served models, each with its identity, limits, capabilities, and status, and the server's own limits and capabilities. |
| `POST /council/v2/contexts` | Builds a context. |
| `GET /council/v2/snapshots/{id}` | A snapshot's served model, tokens, tier, size, and pins. |
| `PUT /council/v2/pins/{owner}`, `GET`, `DELETE` | An owner's pins. |
| `POST /council/v2/specs`, `GET`, `GET /{spec_id}`, `DELETE /{spec_id}` | As v1, without `template`, which a decision's `identity` names. Specs survive a restart. |
| `POST /council/v2/decisions` | Reads a case after each context and pools the reads. |
| `POST /council/v2/reads` | Raw reads: letter log probabilities after a suffix. Capability `reads`. |
| `POST /council/v2/generations` | A sampled reply after a context. Capability `generate`. |

## Served models

```json
GET /council/v2/identity
-> {"models": [
      {"model": "google/gemma-4-26B-A4B-it", "aliases": ["gemma4"], "status": "ready",
       "weight_hash": "0x1EBEA246FC63EB7F", "tokenizer_hash": "sha256:4d777ef5bdc1aa36",
       "template": "mkv-letters-1:a57f…+gemma4-chat:ae53…", "engine": "mkviolet:a93e…",
       "device": "2x Intel(R) Arc(TM) Pro B70 Graphics",
       "limits": {"context_tokens": 65535, "decision_context_tokens": 3072, "snapshot_bytes": 450560,
                  "contexts_per_decision": 8, "default_timeout_ms": 30000},
       "capabilities": ["reads", "generate", "dry_run", "leave_one_out"]},
      {"model": "qwen-3.8-mutant-636", "aliases": [], "status": "unloaded", …}],
    "limits": {"pin_bytes": 17179869184, "case_bytes": 65536, "questions_per_spec": 16, "choice_options": 26},
    "capabilities": ["pins"],
    "extensions": ["/mk/v1"]}
```

- **`models` lists every model the server serves, loaded or not,** so a
  client can tell a name the server never serves from a name it serves
  later. A served model reports the same identity in every status.
- **Every request names its served model with `model`.** `POST /contexts`,
  `/decisions`, `/reads`, and `/generations` require it. It is a served
  model's `model` or one of its `aliases`. A request without `model`, or with
  any other name, is a `400` naming `model`, even when the server serves one
  model. The server never chooses a served model for a client, because a
  client that worked with one served model would break the day a second one
  is added.
- Each response names the served model by its `model`, never by an alias.
- **A served model that is not `ready` gets a `503`** with `error.type`
  `model_unavailable`, `model`, and `status`, before anything runs. A server
  that knows when the model will be `ready` sends `Retry-After` in seconds.
  A `loading` or `unloaded` model takes no requests. A `draining` model
  takes no new requests and finishes the ones it has, open generation streams
  included.
- **Snapshots, pins, and specs belong to the server, not to a served
  model.** `GET /snapshots/{id}`, `/pins`, and `/specs` take no `model`; a
  snapshot names the served model it belongs to. A pin keeps a snapshot
  while its served model is `unloaded`, as it does on any tier; a request
  that reads the snapshot then gets the `503`.
- **A snapshot outlives an unload when the identity comes back unchanged.**
  A server that loads a served model again with every identity field the
  same reads its held snapshots as before. A changed field, such as a new
  `engine` after a rebuild, is a new identity: the server reads none of the
  old identity's snapshots for it.
- **An approximation is a served model of its own.** A server that reads
  with quantized model state, or with snapshots fed in another way, serves
  that under its own `model` and identity, such as `qwen-3.8-mutant-636`. A
  request never asks for an approximation of the model it names, so rule 1
  holds for every served model.
- **`class` and `priority` schedule a request and never change a number.**
  The same request returns the same numbers in either class and at any
  priority. `class` is `interactive`, the default, or `batch`; any other
  value is a `400`. `priority` is a 32-bit signed integer, default 0, and a
  higher priority runs first within its class. All four requests accept
  both; a server without capability `classes` ignores them.
- **A server trusts every client with `priority`.** The contract has no
  authentication, so any client can send a high priority and run before the
  others. A server that serves clients it does not trust leaves out
  capability `classes`.

## Contexts

Every request that reads a context carries it as this object:

```json
{"thinking": true,
 "tools": [{"name": "next_command", "description": "…", "input_schema": {"type": "object", "properties": {…}}}],
 "queue": "next_command",
 "messages": [
   {"role": "system", "content": "You are a policy expert for shell commands. …"},
   {"role": "user", "content": "Commands from a coding agent wait in a queue. …", "snap": true},
   {"role": "assistant", "reasoning": "I should get the first command.",
    "tool_calls": [{"id": "c0", "name": "next_command", "input": {}}]},
   {"role": "tool", "tool_call_id": "c0", "content": "{\"command\": \"ls -la src\"}"},
   {"role": "assistant", "reasoning": "ls only lists. …",
    "tool_calls": [{"id": "c1", "name": "next_command", "input": {"answers": {"verdict": "A"}}}],
    "snap": true}]}
```

`POST /council/v2/contexts` builds a context before its first read, such as
a queue context a client builds at startup:

```json
POST /council/v2/contexts
{"model": "google/gemma-4-26B-A4B-it",
 "context": {"thinking": true, "tools": […], "queue": "next_command", "messages": […]}}
-> {"model": "google/gemma-4-26B-A4B-it", "head": "snap:51d0…", "from": null, "dry_run": false,
    "usage": {"prompt": 1840, "kept": 0, "fed": 1840, "completion": 0, "reasoning": 0},
    "snapshots": [{"id": "snap:9a1e…", "message": 1, "tokens": 412, "tier": "host"},
                  {"id": "snap:51d0…", "message": 4, "tokens": 1840, "tier": "host"}]}
```

- **Messages are the model's chat shape.** Roles are `system`, `user`,
  `assistant`, and `tool`. An assistant message may carry `content`,
  `reasoning` (its thinking), and `tool_calls`; a `tool` message gives the
  result of the call its `tool_call_id` names. The server renders them with its model's own
  template, so a client never writes template tokens.
- **A tool is `{name, description, input_schema}`,** as in Anthropic's
  Messages API: a name no other tool in the context has, an optional
  description, and a JSON Schema object for the call's input. Any other
  member is a `400`. A call is `{id, name, input}`, with `input` a JSON
  object in the member order the model wrote.
- **`is_error: true` on a `tool` message** says the result is the call's
  failure. How it renders is the template's, but a failed result never
  renders the same as the same result without `is_error`. Any other role
  with `is_error` is a `400`.
- **An assistant's text renders before its tool calls,** as the model
  writes them.
- **A system message may come later.** The first message is usually
  `system`. A later `system` message is system text at that point in the
  conversation, such as an instruction from the client's runtime. It is not
  a user message, so it does not end an agent turn: in Gemma 4 it renders as
  a system turn of its own, and the agent turn's thinking before it stays. Like a user
  message, it carries no `reasoning` or `tool_calls`.
- **The server renders thinking where the model was trained to see it.**
  The rule is the template's, so it is part of `template` in identity.
  mkviolet renders Gemma 4 with the template's `preserve_thinking` on: an
  assistant message with tool calls keeps its `reasoning` for the rest of
  the context, so an agent turn renders the same after the next user message
  and a held prefix stays a prefix; the `reasoning` of an assistant message
  without tool calls is not rendered once a later user message follows.
- **`snap: true` marks a boundary** at the end of that message. The server
  renders the context, starts from the longest boundary it holds, feeds
  the rest, and holds a snapshot at each boundary it passes and at the end.
  The response says what it reused: `from` names the snapshot the build
  started from, held from this context or any other, or `null`;
  `usage.kept` counts the tokens it did not feed and `usage.prompt` the
  context's tokens.
- **A boundary exists only where the rendering keeps it.** A marked message
  whose rendering is not a prefix of the whole context is not a boundary,
  and the response lists no snapshot for it. In Gemma 4 this is an
  assistant message with `reasoning` and no tool calls once a later user
  message follows, because the template then drops that reasoning. A marked
  user message or tool message is always a boundary.
- **Every call carries an `id`**, printable text the server never renders, and a
  `tool` message names the call whose result it gives in `tool_call_id`, a
  call of the assistant message before its run of tool messages.
- **`thinking`** absent is the server's default; mkviolet's is on.
- **`queue`** names a tool in `tools`. When the last message is an
  assistant call to it, the context is a queue context: a decision delivers
  the case as that call's result, and the model answers by calling the tool
  again (see Decisions).
- **A changed context is a new context.** A client that edits or drops an
  earlier message sends the new messages; the server starts from the last
  boundary they share with anything it holds. There is no fork request:
  sharing a prefix is the fork.
- **`dry_run: true`** renders and reports `usage` and `from` without
  feeding.
- **`tier`** says where a snapshot's model state is stored: `device`,
  `host`, or `disk`. A read from a `host` or `disk` snapshot loads it first and reports
  the time in `ms.load`.

## Snapshots and pins

```json
GET /council/v2/snapshots/snap:51d0…
-> {"id": "snap:51d0…", "model": "google/gemma-4-26B-A4B-it", "tokens": 1840, "tier": "host",
    "bytes": 829030400, "pins": [{"owner": "kernel-7f3a", "expires_in_s": 3412}]}

PUT /council/v2/pins/kernel-7f3a
{"snapshots": ["snap:51d0…", "snap:c4e2…"], "ttl_s": 3600}
-> {"owner": "kernel-7f3a", "snapshots": [{"id": "snap:51d0…", "held": true},
                                          {"id": "snap:c4e2…", "held": false}],
    "expires_in_s": 3600}
```

- **A snapshot id is `<scheme>:<hex>`:** a scheme of 1 to 16 lowercase
  letters and digits, then 32 to 128 lowercase hex digits, such as `snap:`
  and 64 digits, or `blake3:` and 64 digits. A client treats it as opaque
  text and never derives one. A server derives it from the build's tokens,
  boundaries, and identity (mkviolet), or from the bytes of the model state
  (a content id); either way an id names one model state and is never
  reused for another. When a server's feeding does not repeat bit for bit,
  the same context built twice may get two ids.
- **An owner is any printable name a client chooses,** up to 128
  characters and without a slash, such as a kernel id. It names pins only.
- **`PUT` replaces the owner's whole set** and renews its time, so a client
  that restarts sends its set again and drops what it no longer needs.
  `DELETE` drops the set; `GET` lists it as `{owner, snapshots: [<id>…],
  expires_in_s}`, naming only the snapshots that were held when pinned. An
  owner with no pins is a `404`.
- **A pin keeps a snapshot and the snapshots it is built on** from
  eviction, in memory and on disk. A pinned snapshot may move to a slower
  tier; it is never dropped. A pin on a snapshot the server does not hold
  reports `held: false` and holds nothing: the client builds the context.
- **Pins expire** after `ttl_s` seconds, at most a day, without a `PUT`, so
  a client that goes away leaves nothing held. Pins survive a restart until
  they expire. A snapshot names the owners that pin it, not the ones whose
  pins keep it as part of a chain.
- **The limit is `pin_bytes`** in identity. A `PUT` past it is a `413` that
  pins nothing.

## Decisions

A decision reads one case after each of its contexts with a held spec's
questions, and pools the reads. These parts are as v1 defines them: `ask`
and `options`, `pool`, `timeout_ms`, the answers and their exact relations,
and `leave_one_out`.

```json
POST /council/v2/decisions
{"model": "google/gemma-4-26B-A4B-it", "spec_id": "sha256:9c1f…",
 "case": {"command": "rm -rf build", …},
 "contexts": [{"thinking": true, "tools": […], "queue": "next_command", "messages": […]}]}
```

- **A decision names a held spec with `spec_id`.** It is required; a spec
  the server does not hold is a `404`. A decision has no inline questions:
  wording moves letter probabilities by an order of magnitude, so a question
  is asked only as a spec the client holds, measures, and fits thresholds
  for.
- **`case` is what the decision judges:** a string, an object, or an array,
  at most `case_bytes` long. Any other value is a `400` naming `case`.
- **A decision takes `model`, `spec_id`, `case`, `contexts`, `ask`,
  `options`, `pool`, `timeout_ms`, `think`, `class`, and `priority`.** Any
  other field is a `400`, including the gateway Decisions API's
  `questions`, `session_id`, `user`, `trace`, and `provider`, and v1's
  `state`. A client joins its trace with the `traceparent` header (see
  "Request ids and tracing").

- **`contexts` are context objects,** each sent whole, as above. Each read
  names its context by its index in `contexts`, as do the keys of
  `leave_one_out`, and reports the `snapshot` it read after.
- **The case goes where the context expects it.** For a queue context, it
  is the result of the last call. For any other context, as in v1, it is a
  user message of its own after the spec. Either way, how a spec is
  rendered is part of `template`.
- **The client writes the queue tool's `answers` schema.** The tools render
  at the top of the context, before any spec, so the server cannot add them
  at decision time. Each asked question is a property of
  `input_schema.properties.answers` whose `enum` is the letters of its
  options in order, `A`, `B`, and so on, with the options described in the
  property. The server checks this and otherwise responds with a `400`
  naming `queue`.
- **A queue context's questions are read in the order the model writes one call:**
  in the order the template writes the keys, each after the most likely
  letters of the ones before it. Unlike v1's reads, these answers condition
  each other. A decision's contexts are all queue contexts, or none are.
- **A decision's contexts fit the decision limit,** `decision_context_tokens`
  in identity, which can be shorter than `context_tokens`. mkviolet reads
  decisions in a 4,096-position slot, of which a context may take 3,072.
- **`think`** sets the thought before the answer: `{"tokens": 0}` closes it,
  as v1 always did; `{"tokens": N}` lets the model think greedily, up to N
  tokens, and each read reports its `thought`. Greedy keeps rule 1: the same
  snapshot and case give the same thought and the same numbers. Capability
  `think`.
- **`usage` sums over the reads.** `prompt` counts every token a read was
  conditioned on, and `completion` the `text` answers and thoughts the
  model wrote, with the thoughts also in `reasoning`.

## Raw reads

A raw read is the single forward pass that decisions are built on: letter
log probabilities after a suffix, for clients that write their own
questions.

```json
POST /council/v2/reads
{"model": "google/gemma-4-26B-A4B-it",
 "contexts": [{"thinking": true, "tools": […], "messages": […]}],
 "suffixes": ["<channel|><|tool_call>call:next_command{answers:{verdict:<|\"|>"],
 "letters": ["A", "B", "C"], "top": 5}
```

- Each suffix is read after the context's end.
- **A suffix is the client's own structure**, so its special-token text
  tokenizes as single ids. This is the one place content may be control;
  `signals.control_text` still reports each hit.
- The response names `model` and has one read for each context and
  suffix, with its `context` index, `snapshot`, and `suffix` index, the
  letters' log probabilities as `logprobs`, their `mass`, and the `top`
  tokens as `{token, id, logprob}`, so a client sees where the rest of the
  probability went. Reads are not pooled; a client that pools follows the
  decision rules.
- `usage` sums over the reads: `prompt` counts each context and suffix as
  read, and `completion` is 0.
- **A suffix is written for one template.** When identity's `template`
  changes, a client's suffixes may no longer read what they were written to
  read, so a client keys its suffixes on `template`.

## Generations

```json
POST /council/v2/generations
{"model": "google/gemma-4-26B-A4B-it",
 "context": {"thinking": true, "tools": […],
             "messages": [… , {"role": "user", "content": "how did it go?", "snap": true}]},
 "max_tokens": 2048, "sample": {"temperature": 1.0, "seed": 7}, "stream": true}
```

- **A generation replies after its context.** The server builds the
  context as `POST /contexts` does, then samples the reply. The client
  appends the reply to its own messages for the next request.
- **`max_tokens` is required:** 1 to the served model's `context_tokens`.
  Any other value, or none, is a `400` naming `max_tokens`.
- **`done` is the reply.** It has `model`, the reply's `content`,
  `reasoning`, and `tool_calls`, `finish`, `usage`, `head`, the context's
  `snapshots`, and `seed`. Without `stream`, `done` is the JSON response.
- **`finish`** says why the reply ended:

  | `finish` | When |
  |---|---|
  | `end_turn` | The model ended its turn with no tool call. |
  | `tool_use` | The model ended its turn after one or more tool calls. |
  | `max_tokens` | The reply reached `max_tokens`: `usage.completion` is `max_tokens`. |
  | `context_full` | The prompt and the reply reached `context_tokens`. |

- **`usage`** counts the prompt in `prompt`, `kept`, and `fed`, and the
  reply in `completion`, with its thinking also in `reasoning`.
- **`seed`** in `done` is the seed the server sampled with: the request's
  `sample.seed`, or one the server chose. The same request with the same
  seed usually samples the same reply, but rule 1 does not promise it:
  logits whose last bits depend on how the prompt was fed can change a
  sample. `temperature: 0` is greedy and replays.
- **The server names each call of the reply** with the `id` a later `tool`
  message answers. A parsed call is `{id, name, input}`. A call that does
  not parse is kept as written, `{id, name, raw, error}`; a client that
  sends it back sends it as text.
- **`head` is the snapshot after the reply,** as the reply's message
  renders, when the server holds it. The server holds it when that costs at
  most a few tokens of feeding, so the next request, which sends the reply
  and what follows, starts there. Otherwise `head` is `null` and the next
  request starts from the last boundary.
- **`stats`** in `done`, when a server reports them, describe how the
  sampled tokens looked to the model: for the reply's `reasoning` and
  `content`, and for each tool call as its own `stats`,
  `{tokens, mean_logprob, min_logprob, mean_entropy, max_entropy}`, from
  each token's untempered log probability and the entropy, in nats, of the
  distribution it came from. A part with no tokens has `null`. They are
  diagnostics, not answers: a server may compute them from logits whose
  last bits depend on how the prompt was fed, so replays agree within a
  small tolerance, not bit for bit as rule 1 asks of answers. Nothing means
  anything by them yet; they are logged to calibrate against outcomes.
- **`decisions`** ride with the generation (capability `ride`):
  `[{spec_id, ask?, when}]`, with `when` either `before`, after the prompt,
  or `after`, after the reply. The server asks the held spec's questions in
  a system message after the context or the reply, so the agent turn's
  thinking stays in view. It reads each answer's letter after the most
  likely letters of the questions before it, then discards the model state
  those tokens added, so the reply is the same as without them. `done.decisions` answers each in request order,
  `{spec_id, when, answers}`, each answer in a decision read's shape. A
  spec the server does not hold is a `404`. Like `stats`, the answers are
  the model's account of its own work; nothing means anything by them until
  they are calibrated against outcomes.
- **A prompt that leaves no room is a `413`** with `error.type`
  `context_full`, `prompt_tokens`, and `context_tokens`, before anything
  runs. The client shortens the context.

### Streams

`stream: true` sends server-sent events. Each event's `data` is one JSON
object.

```text
event: token
data: {"id": 1820, "piece": "ls", "phase": "content", "p": 0.91, "h": 0.42, "n": 37, "tps": 48.2}

event: done
data: {"model": "google/gemma-4-26B-A4B-it", "content": "…", "finish": "end_turn", …}
```

| Event | `data` |
|---|---|
| `token` | One sampled token: its `id`; `piece`, the text it adds; `phase`, `reasoning`, `content`, or `call`; `p`, its untempered probability; `h`, the entropy in nats of the distribution it came from; `n`, the reply's tokens so far, this one included; and `tps`, reply tokens per second so far, when the server reports it. |
| `ping` | `{}`. A server may send it at any time, such as while it feeds a long prompt. |
| `done` | The reply, as above. The last event of a stream that succeeds. |
| `error` | An error body, as a status response's: `{error, request_id}`. The last event of a stream that fails; no `done` follows it. |

- **Each completion token is one `token` event.** `n` counts them from 1,
  so a stream that ends with `done` has `usage.completion` `token` events,
  and `usage.reasoning` of them have the phase `reasoning`.
- **A stream ends with exactly one `done` or `error`.** A failure after the
  `200` is sent is an `error` event, never a closed connection without one.
- **`piece` is display text.** It may be empty, such as while a character
  is not yet complete. The reply is `done`'s fields, never the joined
  pieces.
- **A client ignores an event it does not know,** so a server can add one
  without breaking clients.
- **Closing the stream cancels the reply.** The server sends nothing more
  and holds no head for it.

## Request ids and tracing

- **Every response carries a `request-id` header,** a status response, an
  empty one, and a stream alike. The server chooses it: printable ASCII
  without spaces, 1 to 128 bytes, different for each request, such as
  `req_01JB4N8V3Q`. A client treats it as opaque and logs it with the
  request.
- **Every error body carries `request_id`,** the same value as the header,
  beside `error`. An `error` event carries it too.
- **A client joins its trace with the `traceparent` header** (W3C Trace
  Context), and `tracestate` when it has one. The trace context travels only
  as a header: no request or response body carries it, and it never changes
  a number. A server without tracing ignores it.
- **A request id and a trace context are different things.** The server
  names one request with the request id; the client names its own operation,
  which can span many requests and other services, with the trace context.

## Errors

The body is `{"error": {"type", "message", "param"?, …}, "request_id"}`.
`message` leads with the consequence, names the condition, and suggests the
next step when one is known.

| Status | `error.type` | When |
|---|---|---|
| `400` | `invalid_request` | The body is outside the schema, including a missing `model` or an unknown field; `model` names no served model; `ask` or `options` names what the spec lacks; a needed capability is missing. `param` names the field. |
| `404` | `not_found` | An unknown spec id, snapshot id, or pin owner. |
| `413` | `too_large` | A case, spec, or pin set is past a limit in identity; `param` names the limit, such as `case_bytes` or `pin_bytes`. Nothing is held. |
| `413` | `context_full` | A generation's prompt leaves no room for a reply; the body has `prompt_tokens` and `context_tokens`. |
| `429` | `busy` | The server has more requests than it runs. `Retry-After` when known. |
| `500` | `internal` | The server failed. A decision's numbers that fail `gogi-proto`'s check are a `500`, never a response. |
| `503` | `model_unavailable` | The served model is not `ready`; the body has `model` and `status`. `Retry-After` when known. |
| `503` | `unavailable` | The server is stopping or stopped. |
| `504` | `timeout` | A decision's `timeout_ms` passed. No partial answer is returned. |

v1's `409`, `412`, and `507` are gone with context records and owned pins.

## Rules every server keeps

v1's six rules hold: raw numbers replay bit for bit; content is never
control, except a `reads` suffix; each context describes for itself; no
winner is decided; failures are explicit; the rendering is the server's.
In rule 2, `signals.control_text` names the case's hits `case`, not v1's
`state`. In rule 4, `choice` is the argmax for a client to show; it is not
a decision. One more:

7. **A snapshot never changes.** Its id names one model state, made by one
   identity from its tokens and the boundaries along them, so a read from it
   reads exactly the context that made it, and a number never comes from
   history the request did not send.

## Identity, limits, capabilities

`GET /council/v2/identity` returns `models`, `limits`, `capabilities`, and
`extensions`, as "Served models" shows. Each entry of `models` has v1's
identity fields, `model`, `aliases`, `weight_hash`, `tokenizer_hash`,
`template`, `engine`, `device`, and, with `describe`, `text_stop`, plus
`status`, `limits`, and `capabilities`. A limit or capability belongs to the
served model when it depends on the model, and to the server when it depends
on the store or on specs, which every served model shares.

| Limit | Where | Means |
|---|---|---|
| `context_tokens` | served model | The longest context, or a generation's prompt and reply. |
| `decision_context_tokens` | served model | The longest context a decision reads, not counting the spec and the case rendered after it. |
| `snapshot_bytes` | served model | What one token of a snapshot costs. |
| `contexts_per_decision` | served model | The most contexts one decision reads, at most 8. |
| `default_timeout_ms` | served model | A decision's `timeout_ms` when it sends none. |
| `text_tokens` | served model | The most tokens a `text` answer writes, with `describe`. |
| `pin_bytes` | server | The most snapshot bytes all pins together keep, over every served model. |
| `case_bytes` | server | The longest case, in bytes. |
| `questions_per_spec` | server | The most questions a spec holds. |
| `choice_options` | server | The most options a `choice` question has. |

`extensions` lists the server's own route prefixes, such as `["/mk/v1"]`.

| Capability | Where | Means |
|---|---|---|
| `reads` | served model | `POST /council/v2/reads`. |
| `generate` | served model | `POST /council/v2/generations`. |
| `think` | served model | Decisions accept `think` with `tokens` above 0. |
| `ride` | served model | Generations accept `decisions`. |
| `dry_run`, `leave_one_out`, `describe` | served model | As v1. |
| `pins` | server | `/council/v2/pins`. |
| `tiers` | server | Snapshots report `tier`, and pinned snapshots move between tiers. |
| `classes` | server | Requests are scheduled by `class` and `priority`. The server trusts every client with `priority`. |

## Extensions

A server serves the model-specific rest under its own prefix and lists it in
`identity.extensions`. mkviolet keeps `/mk/v1` for these:

| mkviolet route | Why it is not in the council API |
|---|---|
| `render`, `tokenize`, `detokenize` | The template and tokenizer, for debugging and bench tools. |
| `cases` | The last 32 decisions' cases and answers, for labeling. |
| `embeddings` | Gemma 4's last hidden state; quality unmeasured. |
| `status` (event stream), `metrics` | Engine passes per card. |
| `traces` | Residual and attention traces. |
| `jlens`, `steer`, `routing` | Jacobian Lens, residual steering, and MoE routing introspection (mkviolet's `docs/06-council-jlens-extensions.md`). |

## Converting

| From | Changes |
|---|---|
| council v1 | `system` and `turns` become `messages`, with the system message first, and a context travels with each request instead of living under an id. `snap` moves onto messages. Contexts gain `tools`, `thinking`, and `queue`. `pin` and `persist` become owned pins. Decisions gain `think`, send `case` in place of `state`, and lose inline `questions` and the gateway fields. Paths move to `/council/v2`. |
| HIP megakernel `/mk/v1` | Context ids and `POST /contexts` give way to sending the context; the content-hash id becomes the snapshot id. `reads` takes `contexts` and is otherwise the same. `generate`'s `from.context` becomes `generations` with the context. |
| lfm2d `/v1/opinion` | Held specs and describe-first carry over as `specs` and `text` questions. Contexts move from a domain-tagged token hash to messages with `snap` boundaries. |
| mkviolet | One snapshot store for contexts, generations, and decisions; each snapshot stores the KV cache for the tokens after its parent boundary, in memory and on NVMe. `/mk/v1/choices` is gone, replaced by `reads`. v1 contexts and `/mk/v1/generate` keep their own stores. Identity lists its one model under `models`, and a build's response names `model`. |
| kaijutsu | The mk provider sends each chat whole, with `snap` on user messages and tool messages. The council builds one queue context per spec at startup, with a `queue` tool whose `answers` schema is the spec's, and pins it under the kernel's name. Every request names its `model`. Tools and calls move to `{name, description, input_schema}` and `{id, name, input}`. |

## Open questions

- **The cost of sending everything.** Each request renders and tokenizes
  its whole context. On mkviolet, a 32,480-token context renders, checks,
  and returns a `dry_run` response in 79 ms; a server could cache renderings by
  message prefix if that grows.
- **Snapshots across machines.** A client that moves to another server
  sends the same messages; nothing else moves. A server could copy a
  snapshot from another machine when the identity is the same, and content
  ids make that a lookup. Whether the same identity gives the same model
  state on a different device layout, such as one card in place of two, is
  not known; if it does not, the layout belongs in `engine`.
- **A served model per context.** `model` names one served model for a whole
  request. A council that pools reads from different served models would
  name one for each context, and a pooled answer would then have no single
  identity.
- **The queue tool on models without tool training.** A server without a
  tool-call template responds to `queue` with a `400`; whether the server should
  instead deliver the case as a user message, without saying so, is open.
- **Thinking at decision time.** `think` costs about 21 ms a token on
  mkviolet, which does not serve it yet. Whether it pays, and at what
  length, is the bench's next measurement.
- **Riding decisions at each tool call.** `when: "each_call"` would ask
  after each parsed tool call, before the reply goes on, which needs the
  server to pause a decode. Whether it pays depends on what `after` and the
  per-call `stats` show once calibrated.
- **Tool-result size.** A tool result is written whole into a context,
  even when the context then has no room for another message. The client's
  preview and storage rules for large results are its own.
