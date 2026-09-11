//! Metadata extraction for the standalone release-package check.
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::io::Read;

#[cfg(test)]
fn version(bytes: &[u8]) -> Result<String> {
    version_of(bytes, "fux")
}
fn version_of(bytes: &[u8], name: &str) -> Result<String> {
    let metadata: Value = serde_json::from_slice(bytes)?;
    let packages = metadata["packages"]
        .as_array()
        .context("metadata packages")?;
    let mut matching = packages.iter().filter(|package| package["name"] == name);
    let package = matching
        .next()
        .with_context(|| format!("{name} package absent from metadata"))?;
    ensure!(
        matching.next().is_none(),
        "ambiguous {name} package metadata"
    );
    let version = package["version"].as_str().context("package version")?;
    ensure!(
        !version.is_empty()
            && version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b".-+".contains(&byte)),
        "unsafe package version"
    );
    Ok(version.into())
}
/// `package-version [NAME]` prints the workspace version of `NAME` (default `fux`) from
/// `cargo metadata --no-deps` JSON on stdin.
pub fn run(args: Vec<String>) -> Result<()> {
    ensure!(
        args.len() <= 1,
        "package-version takes at most one package name"
    );
    let name = args.first().map_or("fux", String::as_str);
    ensure!(
        !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "unsafe package name"
    );
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 16 * 1024 * 1024, "metadata size limit");
    println!("{}", version_of(&bytes, name)?);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn package_selection_and_invalid_metadata() -> Result<()> {
        assert_eq!(version(br#"{"packages":[{"name":"other","version":"7"},{"name":"fux","version":"0.3.3-beta+1"}]}"#)?, "0.3.3-beta+1");
        for bad in [
            br#"{"packages":[]}"#.as_slice(),
            br#"{"packages":[{"name":"fux","version":"../escape"}]}"#,
            br#"{"packages":[{"name":"fux","version":"1"},{"name":"fux","version":"2"}]}"#,
        ] {
            assert!(version(bad).is_err());
        }
        Ok(())
    }
}
