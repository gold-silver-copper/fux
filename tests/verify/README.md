# Verification corpus

This package is an executable behavioral specification, not a second implementation of fux.
Versioned scenarios live in `corpus/`; reviewed canonical JSONL lives in `fixtures/`; independent
reference models live in `oracle/`; and adapters for production boundaries live in
`interpreters/`. The standalone `fixture-child` crate supplies deterministic PTY behavior without
shell startup files or ambient configuration. Compact concurrency models live in `loom/`.

Normal tests only compare fixtures. To rewrite a reviewed golden explicitly, run:

```sh
tests/verify/record.sh
```

Recording first rejects credential markers, private-key headers, cookies, and real home-path
shapes. All scenario strings, payloads, dimensions, collections, and step counts are validated
before interpretation.

## Fixture child protocol

The fixture child connects to the private Unix socket named by `--control=<path>` (or
`FUX_FIXTURE_CONTROL`) and sends a versioned `ready` JSONL event there. The PTY is reserved solely
for terminal output and application input. Control frames are strict, newline-terminated JSON,
limited to 16 KiB each and 256 commands per run; decoded payloads are limited to 1 MiB. The whole
run has a 10-second deadline by default, adjustable downwards or up to 60 seconds with
`--deadline-ms=<n>`.

The protocol can emit exact split chunks, read exact PTY input, issue a query and read or withhold
its reply, report the real PTY size, emit title/progress/bell/clipboard/OSC 7877 controls, create
exit/ignore-HUP/hold-PTY/wait-signal descendants, fill stdout, refuse stdin, and choose its exit
status. Every orderly exit reports cleanup on the private channel. An RAII owner kills and reaps
all remaining descendants on protocol, I/O, timeout, and disconnect errors.
