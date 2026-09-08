# Retrying after a lost request or reply

Zor's durable operation ID lets a new CLI process reconcile a delivered prompt after its reply
is lost. In this comparison it delivered one line; repeating herdr 0.8.2's same `agent.prompt`
request after the lost reply delivered two. These IDs have different contracts: herdr's request
ID correlates a response, while zor's operation ID retains intent and a fux input receipt. The
result demonstrates a controller capability, not a violation of a shared idempotence promise.

The [harness](../../tools/comparisons/input_retry.py) and [recorded evidence](input-retry.json)
exercise real private servers and Unix-socket forwarding proxies. They reuse the same synthetic
worker from the [prompt-boundary comparison](prompt-boundary.md). Its `claude` executable name
satisfies herdr foreground admission; no real Claude process or credentials are involved.

## Results

Each cell describes three repetitions, with one explicit retry per run:

| Injected fault | Input before retry | herdr final input count | zor final input count |
|---|---:|---:|---:|
| Drop the submission before forwarding to the server | 0 | 1 in all 3 runs | 1 in all 3 runs |
| Drop its reply after the worker consumed the input | 1 | 2 in all 3 runs | 1 in all 3 runs |

All 12 cases passed. Each input count is checked again after the owned server exits and the
worker PID disappears, so it covers the completed run. The before-forward control shows that
retry does deliver an initially missing request in both stacks. The after-consumption case shows
that zor can distinguish an already delivered operation from the unsent operation and avoid
another submission when its receipt remains available.

Herdr's two requests are byte-identical and use fresh connections. The proxy records matching
request SHA-256 hashes and the same ID. Zor's retry uses a new CLI process, the same state directory,
and the same operation ID. Before forwarding was interrupted, the retained fux reservation lets
zor submit that same input operation; after the reply was lost, receipt lookup establishes delivery
without forwarding another input submission. Trace assertions pin the input operation ID in both
cases. Zor's semantic wait remains `pending` and its task outcome remains `open`: delivery evidence
has not been promoted to a response or verified completion.

## Fault placement and limits

The proxy drops only the first submission. Before-forward drops the request without contacting
the upstream server. After-consumption first obtains the upstream reply and waits for the worker's
input log to contain the line, then closes the downstream socket without sending a response.
The caller observes EOF or a CLI transport error, then explicitly retries. This tests a precise
lost-reply boundary, not all possible partial-write/disconnect points.

The multiplexer stays alive throughout each case. Every zor invocation is a separate CLI process,
but there is no killed mid-write caller, service crash, server restart, receipt expiry or eviction,
concurrent human input, remote koh connection, or real-agent execution in this comparison.
Reconciliation occurs within fux's receipt retention window. Zor does not promise durable exactly-once
application processing over terminal input; an application could accept input and perform effects
that its controller cannot independently verify. The test measures the owned worker's line log.

Herdr could be driven by an external controller that adds its own durable journal or refuses retry
after ambiguity. Those policies are not supplied by the tested `agent.prompt` request ID and are
not evaluated here. No latency, setup-cost, detection-quality or universal superiority claim follows
from these counts.

## Provenance and reproduction

The evidence pins both harness and imported helper hashes, worker source, platform, supplied
binary versions/hashes, and the reference HEAD. The herdr binary matches the separately verified
[clean reference build](prompt-boundary-build.json). As in the first comparison, a supplied binary
hash and a reference HEAD alone do not establish their relationship. No reference source or product
implementation changed for this test.

Build the reference using the [canonical-path instructions](prompt-boundary.md#reproduction), then:

```sh
cargo run --manifest-path tools/xtask/Cargo.toml --locked -- capture-input-retry \
  --herdr /absolute/path/to/reference-build/target/debug/herdr \
  --fux target/debug/fux --zor zor/target/debug/zor \
  --herdr-reference references/herdr --repetitions 3 \
  --output /tmp/input-retry-new.json
```

Output must be a new file. Python 3, `/usr/bin/clang`, Unix sockets and private PTY access are
required. The fixture starts and terminates only its own servers with disposable HOME/XDG paths,
uses bounded socket/process waits, and joins the forwarding thread. Product changes that alter
these outcomes fail the pinned baseline assertions and require fresh interpretation.

The first-party capture is now Rust. Historical evidence and exact source archives
remain unchanged. The four-case migration run at
`/tmp/fux-rust-input-retry-capture.json` preserves the lost-request/lost-reply faults,
request-byte and operation identities, exact input counts, and post-cleanup input
verification. New reports record the Rust harness, proxy and shared worker sources.
