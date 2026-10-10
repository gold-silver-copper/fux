//! Compiles libvterm 0.3.3 from the release source `run.sh` fetches (named
//! by `LIBVTERM_SOURCE_DIR`), with the shim `src/engines/libvterm.rs` reads
//! it through, into one static library.
//!
//! One header, written here, is force-included into every source: it
//! renames `abort` and `fprintf` to two functions of the shim that return.
//! libvterm's only live call of either is in `resize_buffer` (screen.c),
//! which aborts the process when a resize loses the cursor; see
//! `libvterm.rs`.
//!
//! And parser.c is compiled bounded (`BOUND`): libvterm 0.3.3 stores a
//! CSI's parameters in 16 slots and counts them without a bound, so a 17th
//! writes past them and crashes the process (an SGR of 17 parameters).
use std::env;
use std::fs;
use std::path::PathBuf;

const SHIM: &str = "src/engines/libvterm_shim.c";

/// libvterm's step to a CSI's next parameter, and the same bounded: past
/// the last slot, each further parameter is written over the last.
const UNBOUNDED: &str = "      if(c == ';') {
        vt->parser.v.csi.argi++;";
const BOUND: &str = "      if(c == ';') {
        if(vt->parser.v.csi.argi < CSI_ARGS_MAX - 1)
          vt->parser.v.csi.argi++;";

const GUARD: &str = "\
/* Written by fux-vt-compare's build.rs; force-included into libvterm. */
#include <stdio.h>
#include <stdlib.h>
void fux_vt_compare_libvterm_abort(void);
int fux_vt_compare_libvterm_fprintf(FILE *stream, const char *format, ...);
#define abort() fux_vt_compare_libvterm_abort()
#define fprintf fux_vt_compare_libvterm_fprintf
";

fn main() -> Result<(), String> {
    println!("cargo:rerun-if-env-changed=LIBVTERM_SOURCE_DIR");
    println!("cargo:rerun-if-env-changed=DOCS_RS");
    // DOCS_RS skips the native build, as it does libghostty-vt-sys's: CI
    // type-checks this crate with it (`cargo clippy`), which links nothing.
    if env::var_os("DOCS_RS").is_some() {
        return Ok(());
    }
    println!("cargo:rerun-if-changed={SHIM}");
    println!("cargo:rerun-if-changed=build.rs");
    let source = env::var_os("LIBVTERM_SOURCE_DIR")
        .map(PathBuf::from)
        .ok_or(
            "LIBVTERM_SOURCE_DIR is not set: build fux-vt-compare through fux-vt/compare/run.sh, \
         which fetches libvterm 0.3.3 and sets it",
        )?;
    let src = source.join("src");
    let mut files = Vec::new();
    for entry in fs::read_dir(&src).map_err(|e| format!("reading {}: {e}", src.display()))? {
        let path = entry
            .map_err(|e| format!("reading {}: {e}", src.display()))?
            .path();
        if path.extension().is_some_and(|ext| ext == "c") {
            println!("cargo:rerun-if-changed={}", path.display());
            files.push(path);
        }
    }
    if files.is_empty() {
        return Err(format!("no libvterm sources in {}", src.display()));
    }
    files.sort();
    let out = PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR is not set")?);
    let parser = src.join("parser.c");
    let text =
        fs::read_to_string(&parser).map_err(|e| format!("reading {}: {e}", parser.display()))?;
    if text.matches(UNBOUNDED).count() != 1 {
        return Err(format!("{}: not libvterm 0.3.3's parser", parser.display()));
    }
    let bounded = out.join("parser.c");
    fs::write(&bounded, text.replacen(UNBOUNDED, BOUND, 1))
        .map_err(|e| format!("writing {}: {e}", bounded.display()))?;
    for file in &mut files {
        if *file == parser {
            file.clone_from(&bounded);
        }
    }
    let guard = out.join("libvterm_guard.h");
    fs::write(&guard, GUARD).map_err(|e| format!("writing {}: {e}", guard.display()))?;
    cc::Build::new()
        .std("c99")
        .include(source.join("include"))
        .include(&src)
        .files(&files)
        .file(SHIM)
        .flag("-include")
        .flag(guard.as_os_str())
        .warnings(false)
        .try_compile("fux_vt_compare_libvterm")
        .map_err(|e| format!("compiling libvterm: {e}"))
}
