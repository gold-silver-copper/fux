# Examples

fuxth programs that drive fux, as a script or an agent would. Each says in
its header what it prints (`# expect:`, a line each) and, if it isn't 0, the
status it exits with (`# status:`); `tests/cassettes/examples.cassette` runs
them all on a private server. Run one on yours with `fux --file examples/NAME.fux`.

The language itself, without fux, has its own examples in
[`fuxth/tests/examples`](../fuxth/tests/examples).

`hooks/program-copies.fux` is an ordinary client program over bounded journal
batches. Give it a saved cursor, a wait in seconds and a batch count:

```sh
C=$(fux all str '' 0 next-events str next get)
fux --file examples/hooks/program-copies.fux "$C" 30 64
```

It filters admitted program copies, sends notices to views, and prints its
next cursor. Save that checkpoint for another finite run. A missed-event
failure requires resnapshotting and reconciliation. The hook example is
exercised against a real program copy in `tests/servers.rs`.
