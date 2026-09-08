# Local attachment measurements

Run the Rust replacement for the historical `tools/measure.py`:

```sh
cargo run --manifest-path tools/xtask/Cargo.toml --locked -- measure target/debug/fux --version 6 --samples 20
```

The version option is explicit because the legacy default remains 3. Use the version
supported by the selected binary. The tool runs a private shell/server, measures startup,
a ten-second idle period, input-marker latency, and a 20,000-line output burst, then
cleans up the server. Output retains the original JSON field names and quantile rule.
CPU and RSS use `ps`; voluntary context switches use `/proc` where available and are
reported as `n/a` elsewhere. No system configuration or permissions are changed.

These remain coarse legacy measurements: process statistics include their platform's
resolution, and marker detection uses visible attachment text, including shell echo.
They do not prove verified agent completion or isolate internal costs. New Rust timings
are not relabelled as historical Python results. The Rust harness uses a clean private
HOME/XDG environment rather than inheriting personal environment overrides.
