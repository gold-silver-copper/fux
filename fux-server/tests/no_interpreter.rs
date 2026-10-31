//! The server never runs fuxth (D-2): what its normal build links is this
//! closed list, which holds no crate with fuxth's interpreter. fuxth-core,
//! the failure codes and grants `fux-wire` shares, is fuxth's one crate in
//! it. A new dependency anywhere below the server changes the list, and is
//! seen here.
use std::collections::BTreeSet;
use std::process::Command;

#[test]
fn the_server_builds_without_the_interpreter() -> Result<(), Box<dyn std::error::Error>> {
    let cargo = std::env::var_os("CARGO").ok_or("cargo runs the tests, and names itself")?;
    let tree = Command::new(cargo)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["tree", "-p", "fux-server", "-e", "normal"])
        .args([
            "--prefix",
            "none",
            "--format",
            "{p}",
            "--locked",
            "--offline",
        ])
        .output()?;
    let text = String::from_utf8(tree.stdout)?;
    assert!(
        tree.status.success(),
        "cargo tree: {}",
        String::from_utf8_lossy(&tree.stderr)
    );
    let linked: BTreeSet<&str> = text.lines().filter_map(|l| l.split(' ').next()).collect();
    let allowed = [
        "fux-server",
        "fux-vt",
        "fux-wire",
        "fuxix",
        "fuxth-core",
        "libc",
    ];
    assert_eq!(linked, BTreeSet::from(allowed));
    Ok(())
}
