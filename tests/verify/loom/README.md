# Concurrency models

Only compact ownership/state machines belong here. PTYs, subprocesses, sockets, and network
implementations are exercised by bounded production tests instead of being simulated with Loom.
