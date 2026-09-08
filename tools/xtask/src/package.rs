//! Metadata extraction for the standalone release-package check.
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::io::Read;

fn version(bytes: &[u8]) -> Result<String> {
    let metadata: Value = serde_json::from_slice(bytes)?;
    let packages = metadata["packages"]
        .as_array()
        .context("metadata packages")?;
    let mut matching = packages.iter().filter(|package| package["name"] == "fux");
    let package = matching
        .next()
        .context("fux package absent from metadata")?;
    ensure!(matching.next().is_none(), "ambiguous fux package metadata");
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
pub fn run() -> Result<()> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 16 * 1024 * 1024, "metadata size limit");
    println!("{}", version(&bytes)?);
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
