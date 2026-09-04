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
