# Koh gateway isolation handoff

Publication update: koh is on `main` at `da712875e4f527b718abe44e9d68f94048e916c7`. The fux companion
pin and gateway-only CI commands now use that commit. The patch and reconstruction proof
below retain the original prepublication evidence; upstream added only CI changes before
the implementation was rebased and pushed.

`koh-gateway-isolation.patch` contains the complete local koh change, including the new
`src/idcmd.rs`. `koh-provenance.json` records its SHA-256, declared upstream/base commit and
byte-identical fresh-checkout reconstruction. These files preserve the original source artifact. The clean
`references/koh` now follows the published commit recorded above.

## Reproduce the koh source

Run from the fux repository root. Choose an unused path for `KOH_CHECKOUT`.

```sh
FUX_CHECKOUT=$(pwd)
KOH_CHECKOUT=/tmp/koh-boundary-review

git clone https://github.com/gold-silver-copper/koh.git "$KOH_CHECKOUT"
git -C "$KOH_CHECKOUT" checkout --detach f6a335237c25aefde9b93290b19a8d909598a95d
git -C "$KOH_CHECKOUT" apply --check "$FUX_CHECKOUT/docs/verification/strict-boundaries/koh-gateway-isolation.patch"
git -C "$KOH_CHECKOUT" apply "$FUX_CHECKOUT/docs/verification/strict-boundaries/koh-gateway-isolation.patch"
git -C "$KOH_CHECKOUT" diff --check
```

Check the patch hash against `koh-provenance.json` using `sha256sum` (Linux) or
`shasum -a 256` (macOS). Do not apply the patch to `references/koh` or relax its clean-pin check.

## Verify the composition with explicit development paths

These commands build the exact local application binaries and prohibit missing-binary skips.
They require the same Rust/native dependencies as each repository's normal build.

```sh
cd "$FUX_CHECKOUT"
cargo +stable build --locked -p fux --bin fux
cargo +stable build --locked -p zor --no-default-features --features cli --bin zor
export FUX_BIN="$FUX_CHECKOUT/target/debug/fux"
export ZOR_BIN="$FUX_CHECKOUT/target/debug/zor"
export KOH_REQUIRE_FUX_BIN=1
export KOH_REQUIRE_ZOR_BIN=1

cargo +stable run --locked --manifest-path tools/xtask/Cargo.toml -- verify-boundaries --koh "$KOH_CHECKOUT"
cargo +stable fmt --manifest-path "$KOH_CHECKOUT/Cargo.toml" --check
cargo +stable clippy --manifest-path "$KOH_CHECKOUT/Cargo.toml" --locked --no-default-features --features cli,gateway --all-targets -- -D warnings
cargo +stable test --manifest-path "$KOH_CHECKOUT/Cargo.toml" --locked --no-default-features --features cli,gateway --lib --test gateway -- --test-threads=1
cargo +stable test --manifest-path "$KOH_CHECKOUT/Cargo.toml" --locked --test pty --test admission --test e2e_loopback --test e2e_pty_binary -- --test-threads=1
cargo +stable package --manifest-path "$KOH_CHECKOUT/Cargo.toml" --locked --allow-dirty --no-default-features --features cli,gateway
cargo +stable package --manifest-path "$KOH_CHECKOUT/Cargo.toml" --locked --allow-dirty
```

If `CARGO_TARGET_DIR` is set, use that directory's `debug/fux` and `debug/zor` paths instead
of the two default target paths above. `--allow-dirty` packages the review patch locally;
it neither commits nor publishes anything.

## Publication procedure (completed for the revision above)

1. Publish the reviewed koh source through the upstream repository's normal process.
   A local-only commit or this patch is not a valid companion pin.
2. Once its exact commit is fetchable from the declared repository, update only the koh
   commit in `tools/xtask/companions.json` to that published revision. Keep repository/path
   provenance intact.
3. For an existing reference checkout, first confirm `git -C references/koh status --short`
   is empty, then fetch the published revision and check it out detached. Run
   `fux-xtask dependencies verify` against the updated pin. `dependencies apply` clones a
   missing reference checkout; it deliberately refuses to retarget an existing checkout
   whose HEAD differs from the pin. Do not copy the dirty development checkout over it.
4. In `.github/workflows/ci.yml`, add `--no-default-features --features cli,gateway` to both
   gateway test commands in the explicit composition job. Add
   `cargo run --locked --manifest-path tools/xtask/Cargo.toml -- verify-boundaries --koh references/koh`.
   Retain the separate default-shell/admission tests and all required-binary flags.
5. Run the pinned composition locally and inspect hosted CI after publication. Configure
   required status checks through the repository's normal administration process if needed;
   adding a job alone does not configure branch protection.

The ordinary composition job now exercises the published pin's gateway-only build. Detailed verification results and publication state are in
[the implementation report](../../strict-boundary-implementation.md).
