//! Test-only OpenCode plugin substitution followed by exec of the real agent.
use anyhow::{Context, Result};
use serde_json::Value;
use std::{ffi::OsString, os::unix::process::CommandExt, process::Command};
fn substitute(config: &str, wrapper: &str) -> Result<(String, String)> {
    let mut config: Value = serde_json::from_str(config)?;
    let last = config
        .get_mut("plugin")
        .and_then(Value::as_array_mut)
        .and_then(|plugins| plugins.last_mut())
        .context("missing final adapter plugin")?;
    let original = last.as_str().context("adapter plugin path")?.to_owned();
    *last = wrapper.into();
    Ok((original, serde_json::to_string(&config)?))
}
pub fn run(args: Vec<OsString>) -> Result<()> {
    let (agent, args) = args
        .split_first()
        .context("usage: reload-opencode-fixture AGENT [ARGS]")?;
    let (plugin, config) = substitute(
        &std::env::var("OPENCODE_CONFIG_CONTENT").context("OpenCode fixture config")?,
        &std::env::var("ZOR_FIXTURE_WRAPPER").context("fixture wrapper")?,
    )?;
    // exec keeps the registered foreground PID and preserves the supplied argv exactly.
    let error = Command::new(agent)
        .args(args)
        .env("ZOR_FIXTURE_PLUGIN", plugin)
        .env("OPENCODE_CONFIG_CONTENT", config)
        .exec();
    Err(error).context("exec real OpenCode fixture")
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn substitutes_only_the_final_plugin_and_preserves_other_configuration() -> Result<()> {
        let config = json!({"plugin":["probe.mjs","file:///adapter with spaces.mjs"],"model":"fixture/fixture","nested":{"value":[1,true]}});
        let (original, rewritten) = substitute(&config.to_string(), "file:///reload.mjs")?;
        assert_eq!(original, "file:///adapter with spaces.mjs");
        let mut expected = config;
        expected["plugin"][1] = "file:///reload.mjs".into();
        assert_eq!(serde_json::from_str::<Value>(&rewritten)?, expected);
        for invalid in [
            "{}",
            r#"{"plugin":[]}"#,
            r#"{"plugin":[null]}"#,
            "malformed",
        ] {
            assert!(substitute(invalid, "wrapper").is_err());
        }
        Ok(())
    }
}
