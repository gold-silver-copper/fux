# Security

Any process that can read the 0600 token file `$XDG_RUNTIME_DIR/fux/<server>.brp.json` has
full authority over that server, exactly as a process that could connect to the old 0600
socket did. Kernel peer credentials are gone with the socket: the BRP listener is plain HTTP on
`127.0.0.1`, so per-connection identity is the token a request carries, not a UID. The
listener binds loopback only and its responses set no CORS allowances (`with_headers` is never
used); a browser "simple request" can reach the port but cannot present the token, and DNS
rebinding likewise reaches the port without the token. The same holds for the attachment
stream (prompt 3.10): loopback, its own token in the same file, no peer credentials.

## Tokens

* **Server token.** 256 bits from `/dev/urandom` as 64 hex characters, minted when the
  `RemoteControlPlugin` builds and written into `brp.json` beside the port, the attachment
  endpoint, the process id and the instance nonce. The file is created `0600` under a
  directory that `paths::private_dir` made `0700` and verified to be owned by the user with no
  group/world bits; a reader (`client::read_descriptor`) refuses a descriptor that is not a
  regular file with mode `0600`. The token is never persisted anywhere else and dies with the
  process (`DescriptorGuard` removes the file on shutdown).
* **Every method checks it.** `bevy_remote` hands handlers `BrpMessage { method, params,
  sender }` and nothing else — no headers, no peer — so authority lives in `params.token`,
  compared in constant time against every stored token without short-circuiting on the first
  match (`Tokens::authorize`). A request without a token, with an unknown token, or with a
  token lacking the capability is answered `unauthorized` before any handler side effect; the
  BRP integration tests assert that. The method table is fux's allowlist only: the mutating
  `world.*` built-ins are not registered, the read built-ins are token-checking wrappers over
  reflected projections, and authoritative components are absent from the type registry.
* **Mutations carry the instance nonce.** `instance` must equal the running server's nonce;
  a stale one (another incarnation) is refused, so a client that reads an old `brp.json` or
  replays across a restart never mutates the wrong server. Layout edits additionally carry
  `LayoutGeneration`.
* **Minted tokens.** `fux/token.mint { workspace?, capabilities }` needs the `admin`
  capability, so in practice the server token or a token minted with `admin`. Capabilities are
  `read`, `mutate`, `attach`, `admin`; a minted token cannot exceed the minter's grant and may
  be scoped to one workspace, after which workspace-wide operations (`workspace.list`,
  `workspace.new`, anything touching another workspace) are refused. The table is bounded by
  `[limits].tokens` (default 256) and never persisted: a restart forgets every minted token and
  `session.restore` cannot bring one back.
* **Revocation.** `fux/token.revoke { revoke }` (`admin`) removes a minted token at once; the
  server token cannot be revoked. Every open `+watch` stream remembers the token that opened
  it, and revocation ends those streams in the same update (`watch::revoke`), so a revoked
  holder receives nothing further — not even events already queued for its stream.

## Attachment stream

Viewers attach on the second loopback listener with `Hello { token, instance, workspace,
stream, viewport, exact_target? }` as the first frame. Its token is a separate 256-bit value
published in the same `brp.json` (`attach.token`), so the file's `0600` is the whole boundary
again; the instance nonce must match, both compared in constant time. A connection that sends
no valid `Hello` within 5 s is closed; frames are length-prefixed and capped at 8 MiB;
connections are capped at `2 * [limits].viewers` with the OS backlog as backpressure, so an
unauthenticated peer cannot grow tasks or queued handshakes. Admission beyond the token (the
workspace exists, an exact target is a live pane) is the World's decision, answered with a
`Bye` reason. The `attach` capability bit of the BRP token vocabulary is not consulted by the
stream: attachment authority is the attachment token, not a minted BRP token.

## Resource exhaustion

Measured in `crates/fux/tests/brp_exhaustion.rs` (the numbers, with the run conditions, are
in `docs/verification.md` under "BRP resource exhaustion"). Against `bevy_remote`'s
`RemoteHttpPlugin`:

* a 64 MiB request body is buffered whole (+122 MiB RSS while held, +187 MiB after the reply's
  copy) — one request, no token needed to send it;
* a 10,000-element batch is dispatched one round trip at a time and costs 13–27 s of server
  time (1.3–2.7 ms per element in a debug build), during which other requests still answer in
  under 65 ms because dispatch interleaves;
* 1,000 idle connections are all accepted and served by one task each (+14–36 MiB), reaped
  only by hyper's 30 s header timeout; a 1,001st request answers in 7–13 ms — with a
  sufficient descriptor limit. Under the default macOS shell limit of 256 descriptors the
  accept loop returns on `EMFILE` after 120 connections (both ends count in the test process;
  about twice that from another process) and the BRP listener is gone for the life of the
  process (every later connect is refused), which no token holder can undo;
* a stalled body (headers only) is held with no deadline at all — still open at 36 s — while
  other requests answer in 3–16 ms.

The idle-connection result fails the prompt's contract in the default environment, so fux
ships the prompt's fallback as the default transport (`HttpTransport::Bounded`,
`crates/fux/src/remote/http.rs`): the same hyper/`smol-hyper` service feeding the same
`BrpSender` mailbox, with a **1 MiB body limit** (an oversized body is discarded rather than
buffered and answered `413`, so the client sees the refusal instead of a reset), a **64-request
batch limit** (`-32600` under a null id), a **256-connection cap** with the OS backlog as
backpressure (the attachment listener's pattern), **10 s deadlines** for the request head and,
separately, the body (`408`), and an **accept loop that backs off on `EMFILE`** instead of
losing the listener. Measured: a 64 MiB body streams through in 63 ms with RSS flat; a
10,000-element batch is `413` (2.1 MiB) and 65 elements are refused as a batch; 1,000 idle
connections hold the 256 slots for 10 s and are then reaped, after which the next request
answers, and under 256 descriptors the listener survives and answers 10 s later; a stalled
body is `408` at 10.0 s. `RemoteHttpPlugin` stays selectable (`HttpTransport::BevyRemote`) for
comparison and as the upstream reference; it is not forked.

### Not mitigated, and why

* **Holding the connection cap.** Any local process — no token — can open 256 idle
  connections and hold them for up to 10 s, and reconnect as they are reaped. During that
  window new connections are refused by the kernel (`ECONNRESET` on macOS once the 128-entry
  backlog is full; a SYN wait on Linux), so BRP is unavailable to everyone else, including
  zor. This is the trade a fixed cap makes; the alternative, evicting the oldest idle
  connection, needs per-connection bookkeeping inside hyper's serve loop and is not worth it
  for a loopback-only listener whose peers are the user's own processes. The listener always
  recovers; nothing is lost but time.
* **Spending server time with the token.** A token holder can send 64-element batches back to
  back or 1 MiB bodies at line rate; each is answered in order through the single mailbox and
  the runner's per-update batch (`MAILBOX_SIZE` 64, `DISPATCH_BATCH` 1024). Loopback plus the
  token limits *who* can spend that time, not *how much*: the holder already has full
  authority over the server, so a rate limit would only bound a client that could instead
  call `pane.close`.
* **Descriptor limits.** fux does not raise `RLIMIT_NOFILE`; a server started from a shell
  with the macOS default of 256 has room for the 256 BRP slots plus its PTYs and viewers only
  because idle BRP slots are reaped. Raising the soft limit at startup is a deployment choice
  left to the launcher.
* **The attachment stream's cap** follows `[limits].viewers` and applies before
  authentication, with the same hold-the-cap exposure and the same 5 s handshake deadline as
  the bound above.

## Files on disk

| file | mode | contents |
|---|---|---|
| `$XDG_RUNTIME_DIR/fux/` | `0700`, owner-checked | descriptors |
| `$XDG_RUNTIME_DIR/fux/<server>.brp.json` | `0600` (temp + rename) | port, server token, attachment endpoint and token, instance nonce, pid |
| `$XDG_STATE_HOME/fux/` | `0700`, owner-checked | state |
| `$XDG_STATE_HOME/fux/session/<server>.scn.ron` | `0600` (temp + fsync + rename) | the allowlisted layout subgraph plus `LaunchAttribution`, `Name`, focus, cwd and a bounded screen history; never sockets, PTY descriptors, tokens or the nonce |
| `$XDG_CONFIG_HOME/fux/layouts/<name>.scn.ron` | `0600` under a `0700` directory | user layouts; the same allowlisted vocabulary |
| `$XDG_STATE_HOME/zor/journal.scn.ron` | `0600` (temp + fsync + rename + directory sync) | zor's persisted subgraph, bounded at extraction |
| `$XDG_STATE_HOME/zor/archive/<date>.scn.ron` | `0400` (rewritten through a temp file when merged) | archived closed tasks, read-only |

Session and journal files hold no credentials: the extraction allowlists are explicit, and the
session test asserts the written document contains neither the token nor the nonce. A
`session.restore` re-materialises processes through the same validated template path as a
client request, so a hand-edited session file can at most describe panes the user could have
asked for.
