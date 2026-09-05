# Ownership refactor completion

Objective: execute [project-boundaries-refactor-prompt.md](project-boundaries-refactor-prompt.md).
Implementation, verification, separate diff review, and the requirement audit are complete.
See [refactor-verification.md](refactor-verification.md) for contracts, evidence, review fixes,
reproduction commands, and the native-platform/release limitations.

## Completed requirements

- [x] koh embedding API, opaque identities, admission/reconnect/cancellation ownership.
- [x] fux has no direct transport/key implementation or iroh dependency.
- [x] fux owns pane PTYs, process groups, terminal I/O and application cleanup.
- [x] Shared command dispatch and authoritative binding/help registry, including remote/reloaded
      viewer shortcuts, literal embedded input, and byte-alias validation.
- [x] Versioned and bounded zor observation reports, documented semantics and provenance.
- [x] Prompt-count, distinct-passphrase, switching, cancellation, terminal-restoration and reset
      safety regressions pass with isolated disposable keys and PTYs.
- [x] Reproducible dependency patches, immutable bases, CI assembly, and reconstructed build/test.
- [x] Architectural guards and public cross-project contract/lifecycle tests.
- [x] Native formatting, linting, test, documentation and relevant feature/MSRV checks.
- [x] Separate full-diff review, confirmed fixes, affected checks, and final requirement audit.

## Final evidence

- fux full serial all-feature suite passed; final affected configuration/client/host/architecture
  checks passed after review fixes. The host suite contains 43 passing tests.
- Installed-style binary corpus: all 8 scenarios passed, covering real terminal behavior,
  concurrent startup, cancellation phases, reconnect/retention, observation and process cleanup.
- Interactive key CLI suite passed; koh identity transfer/lease, admission/capacity/cancellation,
  literal-input, concurrent-creation and relative-reset contract tests passed.
- fux `fux/2` wire-schema check rejects `fux/1` peers before admission to an incompatible state.
- Independent koh full tests, strict Clippy, alternate backends/no-CLI checks and docs passed.
- Independent zor full/no-default-feature tests, Rust 1.91 linting, docs and local package
  verification passed. One earlier signal-test timing failure did not recur in 15 focused runs;
  the observed intermittent risk is retained in the verification report.
- fux Rust 1.91 all-target type check, fixture Clippy and final formatting/diff checks passed.
- Final `python3 tools/dependencies.py verify --build` completed successfully: reconstructed source,
  compiled binaries, 43 host tests, client tests and required real-zor integration all passed.
- Dependency apply was checked for idempotence and rejection of additional divergent files.

## Source delivery

The owner changes are committed in koh at `d6ded15bca6fb807b4896f49c8a43dfcdf43ee27` and zor at
`25cbc462ee3cf91034fa6763279662f0e2eaabc7`. The dependency manifest and CI checkouts pin those
commits; dependency patches are empty because the changes now live in their owning repositories.
The user subsequently authorized committing and pushing all three repositories.
No packages were published and no personal keys or sessions were modified.
The dependency APIs require the provided development assembly until independent releases exist;
native tests were run on macOS, not Linux/Android devices.
