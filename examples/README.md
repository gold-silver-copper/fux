# Examples

grift programs that drive fux, as a script or an agent would. Each says in
its header what it prints (`# expect:`, a line each) and, if it isn't 0, the
status it exits with (`# status:`); `tests/cassettes/examples.cassette` runs
them all on a private server. Run one on yours with `fux --file examples/NAME.grift`.

The language itself, without fux, has its own examples in
[`grift/tests/examples`](../grift/tests/examples).
