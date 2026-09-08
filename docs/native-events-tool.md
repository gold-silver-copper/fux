# Native OpenCode event capture

Run the real OpenCode TUI through fux with a deterministic loopback provider:

```sh
cargo run --manifest-path zor/tools/xtask/Cargo.toml --locked --bin zor-xtask -- \
  capture-opencode-events --fux target/debug/fux --opencode /path/to/opencode \
  --output /tmp/new-native-events
```

The output directory must be new. The capture uses private HOME/XDG configuration,
fixed fixture prompts, an owned read fixture and no cloud credentials. It records
native hooks, two distinct input operations, delivered receipts, the intermediate
read-tool stop, final correlated text and later idle events. These demonstrate the
native adapter contract, not model quality, zor report binding or task success.

The Rust capture reuses the existing native-event validator. `diagnostic.json` is
written after process/provider cleanup; only a successful validated capture writes
`events.json`. New reports identify the actual executable as `rust-binary` and hash
it, OpenCode, fux and the exact probe plugin. Retained historical reports are unchanged.

The original Python capture and its last integration importer have been removed.
Their exact historical sources remain as nonexecutable provenance archives. Current
startup, native-event, managed-integration and resume capture commands run Rust.
