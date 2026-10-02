//! Compiles libvterm 0.3.3 from the release source `run.sh` fetches (named
//! by `LIBVTERM_SOURCE_DIR`), with the shim `src/engines/libvterm.rs` reads
//! it through, into one static library.
//!
//! One header, written here, is force-included into every source: it
//! renames `abort` and `fprintf` to two functions of the shim that return.
//! libvterm's only live call of either is in `resize_buffer` (screen.c),
//! which aborts the process when a resize loses the cursor; see
//! `libvterm.rs`.
use std::env;
use std::fs;
use std::path::PathBuf;

const SHIM: &str = "src/engines/libvterm_shim.c";

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
