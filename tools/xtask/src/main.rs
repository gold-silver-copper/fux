//! Development evidence tools, separate from zor's runtime and protocol library.
mod authenticated;
mod capture_authenticated;
mod capture_integration;
mod capture_lifecycle;
mod capture_native;
mod capture_resume;
mod capture_startup;
mod capture_zor_resume;
mod integration;
mod lifecycle;
mod native_events;
mod native_provider;
mod reload_fixture;
mod resume;
mod resume_proxy;
mod runtime;
fn main() -> std::process::ExitCode {
    let result = match std::env::args().nth(1).as_deref() {
        Some("capture-opencode-resume") => capture_resume::run(std::env::args().skip(2).collect()),
        Some("reload-opencode-fixture") => {
            reload_fixture::run(std::env::args_os().skip(2).collect())
        }
        Some("verify-opencode-resume") => resume::native_regressions(),
        Some("capture-zor-resume") => capture_zor_resume::run(std::env::args().skip(2).collect()),
        Some("verify-zor-resume") => resume::zor_regressions(),
        Some("capture-opencode-integration") => {
            capture_integration::run(std::env::args().skip(2).collect())
        }
        Some("verify-opencode-integration") => integration::regressions(),
        Some("capture-opencode-events") => capture_native::run(std::env::args().skip(2).collect()),
        Some("verify-opencode-events") => native_events::regressions(),
        Some("verify-lifecycle") => lifecycle::regressions(),
        Some("verify-codex-authenticated") => authenticated::codex_regressions(),
        Some("verify-claude-authenticated") => authenticated::claude_regressions(),
        Some("capture-lifecycle") => capture_lifecycle::run(std::env::args().skip(2).collect()),
        Some("capture-startup") => capture_startup::run(std::env::args().skip(2).collect()),
        Some("capture-codex-authenticated") => capture_authenticated::run(
            capture_authenticated::Kind::Codex,
            std::env::args().skip(2).collect(),
        ),
        Some("capture-codex-approval") => capture_authenticated::run(
            capture_authenticated::Kind::Approval,
            std::env::args().skip(2).collect(),
        ),
        Some("capture-claude-authenticated") => capture_authenticated::run(
            capture_authenticated::Kind::Claude,
            std::env::args().skip(2).collect(),
        ),
        _ => Err(anyhow::anyhow!(
            "usage: zor-xtask verify-lifecycle|verify-codex-authenticated|verify-claude-authenticated\n       zor-xtask capture-lifecycle|capture-startup --fux PATH --agent PATH --output NEW_DIRECTORY\n       zor-xtask capture-codex-authenticated|capture-codex-approval --fux PATH --agent PATH --auth-file PATH --model MODEL --output NEW_DIRECTORY\n       zor-xtask capture-claude-authenticated --fux PATH --agent PATH --model MODEL --output NEW_DIRECTORY"
        )),
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("zor-xtask: {error:#}");
            if error.downcast_ref::<capture_startup::Usage>().is_some() {
                std::process::ExitCode::from(2)
            } else {
                std::process::ExitCode::FAILURE
            }
        }
    }
}
