# Owned-process resource sampling

The macOS native sampler works in this sandbox despite `ps` failing with `Operation
not permitted`. This establishes a usable measurement method; it is not a fux/zor
versus herdr performance result.

Reproduce from the repository root:

```sh
cargo run --manifest-path tools/xtask/Cargo.toml --locked -- resource-sampler-check --output /tmp/resource-sampler-check.json
```

The harness compiles `resource_sampler.c` with `clang -Wall -Wextra -Werror`, runs
three 200 ms CPU calibrations, rejects six invalid PID arguments, and samples an
owned Rust process before and after it touches a 32 MiB allocation. Readiness,
compilation, sampling and cleanup are bounded. No account credentials are used.
The retained JSON includes the sampler source hash, platform, raw readings and
normal child exit.

In the historical retained Python-worker run, converting raw CPU counters by the Mach timebase (125/3)
produced ratios of 0.999921–0.999943 against `CLOCK_PROCESS_CPUTIME_ID`. RSS grew
by 33,587,200 bytes and physical footprint by 33,603,608 bytes. Allocation/runtime
bookkeeping can make growth exceed the requested 33,554,432 bytes. Assertions
require at least 24 MiB growth and CPU ratios within 5%; these are measurement
sanity checks, not benchmark tolerances.

For subsequent comparisons, sample only owned process IDs, verify the process
start counter remains constant across samples, and check owner liveness. Convert
counter deltas with the returned timebase. Include fux and zor as separate
components against herdr's integrated process, and disclose excluded workers and
harnesses. Summed RSS is not unique physical memory because shared pages can be
counted more than once. The sampler does not count capture requests, descendant
resources, or peak memory between samples. Calibrate again in each benchmark run.

Paired idle/burst CPU, memory, capture traffic, latency and pane scaling remain
unmeasured by this artifact.
