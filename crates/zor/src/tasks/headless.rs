//! Provider command policy for a single noninteractive launch, owned entirely by zor.
use anyhow::Result;

/// Expand a program/prompt pair before recording ordinary durable launch intent.
/// This supplies no native receipt binding and does not infer task completion.
pub fn argv(agent: Option<&str>, argv: Vec<String>, integrated: bool) -> Result<Vec<String>> {
    let Some(agent) = agent else { return Ok(argv) };
    anyhow::ensure!(
        !integrated,
        "headless launch cannot be combined with a managed native integration"
    );
    anyhow::ensure!(
        argv.len() == 2,
        "headless launch requires exactly an executable and one prompt; extra provider flags are unsupported"
    );
    let [program, prompt]: [String; 2] = argv
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid headless argv"))?;
    anyhow::ensure!(
        !prompt.trim().is_empty() && prompt != "-",
        "headless launch requires a literal nonempty prompt, not stdin"
    );
    let options: &[&str] = match agent {
        "codex" => &[
            "--ask-for-approval",
            "never",
            "exec",
            "--json",
            "--sandbox",
            "workspace-write",
            "--color",
            "never",
            "--skip-git-repo-check",
            "--",
        ],
        "claude" => &[
            "--print",
            "--output-format",
            "stream-json",
            "--verbose",
            "--permission-prompts",
            "none",
            "--",
        ],
        _ => anyhow::bail!(
            "unsupported-headless-agent: only codex and claude have a built-in noninteractive launch policy"
        ),
    };
    let mut command = vec![program];
    command.extend(options.iter().map(|arg| (*arg).to_owned()));
    command.push(prompt);
    super::launch::validate_argv(&command)?;
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn headless_launch_preserves_literal_prompt_and_rejects_ambiguous_modes() -> Result<()> {
        for agent in ["codex", "claude"] {
            let command = argv(
                Some(agent),
                vec!["/tmp/provider".into(), "--not-a-flag $(literal)".into()],
                false,
            )?;
            assert_eq!(command.first().map(String::as_str), Some("/tmp/provider"));
            assert!(command.ends_with(&["--".into(), "--not-a-flag $(literal)".into()]));
            assert!(argv(Some(agent), vec!["provider".into(), "-".into()], false).is_err());
            assert!(
                argv(
                    Some(agent),
                    vec!["provider".into(), "prompt".into(), "--interactive".into()],
                    false
                )
                .is_err()
            );
            assert!(argv(Some(agent), vec!["provider".into(), "prompt".into()], true).is_err());
        }
        assert!(
            argv(
                Some("opencode"),
                vec!["provider".into(), "prompt".into()],
                false
            )
            .err()
            .ok_or_else(|| anyhow::anyhow!("unsupported headless agent accepted"))?
            .to_string()
            .starts_with("unsupported-headless-agent:")
        );
        let plain = vec!["/bin/cat".into()];
        assert_eq!(argv(None, plain.clone(), false)?, plain);
        Ok(())
    }
}
