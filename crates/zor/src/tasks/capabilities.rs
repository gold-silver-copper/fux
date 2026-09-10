//! Static adapter guarantees, separate from live endpoint availability and task success.
use serde_json::{Value, json};

fn capability(level: &str, reason: &str) -> Value {
    json!({"level":level,"reason":reason})
}

/// Read-only and account-independent. Availability must be sampled with adapter-status.
/// A provider's upstream API is not a capability until zor implements its adapter.
pub fn inspect(agent: &str) -> anyhow::Result<Value> {
    if !matches!(agent, "opencode" | "claude" | "codex") {
        anyhow::bail!("unsupported-agent: no built-in adapter capability contract for {agent}");
    }
    if agent == "codex" {
        return Ok(codex());
    }
    let native = agent == "opencode";
    let passive = capability(
        "passive",
        "Terminal/process observations are fallback evidence; they do not bind a native message to submitted input",
    );
    let unavailable = capability(
        "unavailable",
        "Zor has no managed native adapter for this provider; upstream stream/session options alone do not establish support",
    );
    Ok(json!({
        "schema":1,"agent":agent,"scope":"implemented-zor-contract",
        "availability":"not-probed","verified_task_completion":false,
        "capabilities": {
            "launch":capability("supported", "task start retains launch intent and reconciles the generic fux launch receipt; caller supplies argv"),
            "headless-launch":if native { capability("unavailable", "This single-prompt launch policy covers Claude/Codex; OpenCode uses its separate managed integration") } else { capability("supported", "task start --headless-agent accepts EXECUTABLE PROMPT and records noninteractive JSON-mode argv with permission prompts disabled; native receipt correlation remains unavailable") },
            "discovery":capability("passive", "watch/status discover local panes and classify process/terminal evidence; unrecognized state stays unknown"),
            "native-correlation":if native { capability("native", "Managed OpenCode integration binds the armed prompt receipt to native session/message ancestry") } else { unavailable.clone() },
            "working":if native { capability("native", "Registered OpenCode observations are identity-checked and invalidated with observation freshness") } else { passive.clone() },
            "input-required":if native { capability("native", "Managed OpenCode reports prompt-scoped needs-input evidence; this is not task success") } else { passive.clone() },
            "response":if native { capability("native", "Managed OpenCode reports a response associated with the bound native input message; this is not task verification") } else { passive },
            "native-interrupt":capability("unavailable", "No native turn-interrupt operation is implemented; task cancel only cancels coordination and task stop closes an owned pane"),
            "native-session-recreation":if native { capability("native", "task resume requires an open task with a reconciled lost/finished attempt, original-process absence, retained native session/storage metadata and a distinct operation ID; stop intent is rejected") } else { unavailable },
            "cancel-coordination":capability("supported", "task cancel does not retract queued input or signal an adopted pane"),
            "stop-owned-worker":capability("supported", "task stop reconciles closure of only the pane owned by this managed launch")
        },
        "retry":"Reuse the same operation ID and identical intent; reconcile uncertain receipts before sending new input",
        "evidence":"This contract reports implemented paths, not live provider validation; inspect retained coverage separately"
    }))
}

fn codex() -> Value {
    json!({
        "schema":1,"agent":"codex","scope":"implemented-zor-contract",
        "availability":"not-probed","verified_task_completion":false,
        "capabilities": {
            "launch":capability("supported", "task codex-start owns an app-server stdio child inside a managed fux pane; caller supplies executable and argv"),
            "headless-launch":capability("supported", "task codex-start initializes a persistent native thread with approvalPolicy never; required validation uses account-free fixtures"),
            "discovery":capability("native", "task codex-inspect reports retained registered thread, producer and input identities; this is not a probe of live provider availability or discovery of arbitrary external Codex sessions"),
            "native-correlation":capability("native", "task codex-submit binds stable literal input to native client, thread, turn and user-item identities; an acknowledgement alone is insufficient"),
            "working":capability("native", "Correlated native turn events and explicitly requested history provide working evidence; retained evidence is not a live availability probe"),
            "input-required":capability("native", "Native server requests retain a structured blocker for the correlated active turn; this is not task success or permission to approve the request"),
            "response":capability("native", "Completed native response items retain bounded text and full-text hashes for the correlated input; partial interrupted snapshots do not prove completed response output"),
            "native-interrupt":capability("native", "task codex-interrupt queues a distinct stable control for the correlated turn; RPC acknowledgement remains separate from observed native interruption"),
            "native-session-recreation":capability("native", "task codex-recreate requires a live owned wrapper and materialized retained storage; it stops the old provider before resuming the same thread and storage with a new producer, without replaying input; lost-wrapper restart is unsupported"),
            "cancel-coordination":capability("supported", "task cancel prevents new input but does not interrupt a native turn or stop its worker; explicit native controls remain separate"),
            "stop-owned-worker":capability("supported", "task stop closes the owned fux pane; the wrapper stops its owned app-server process and retires journal authority")
        },
        "retry":"Reuse identical input/control intent and stable IDs; task codex-reconcile reads history to resolve lost events without granting another input send; original deadlines remain unchanged",
        "evidence":"Production adapter paths pass deterministic fixtures and real-fux process tests; installed initialization and storage metadata were inspected separately. No live model turn or materialized real-provider resume is verified"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passive_providers_do_not_inherit_native_or_completion_claims() -> anyhow::Result<()> {
        for agent in ["claude"] {
            let value = inspect(agent)?;
            assert_eq!(value.get("availability"), Some(&json!("not-probed")));
            assert_eq!(value.get("verified_task_completion"), Some(&json!(false)));
            for operation in [
                "native-correlation",
                "native-interrupt",
                "native-session-recreation",
            ] {
                assert_eq!(
                    value.pointer(&format!("/capabilities/{operation}/level")),
                    Some(&json!("unavailable"))
                );
                assert!(
                    value
                        .pointer(&format!("/capabilities/{operation}/reason"))
                        .and_then(Value::as_str)
                        .is_some_and(|reason| reason.len() > 20)
                );
            }
            assert_eq!(
                value.pointer("/capabilities/working/level"),
                Some(&json!("passive"))
            );
        }
        assert_eq!(
            inspect("opencode")?.pointer("/capabilities/native-correlation/level"),
            Some(&json!("native"))
        );
        assert_eq!(
            inspect("opencode")?.pointer("/capabilities/native-interrupt/level"),
            Some(&json!("unavailable"))
        );
        assert!(
            inspect("unknown")
                .err()
                .ok_or_else(|| anyhow::anyhow!("unknown agent accepted"))?
                .to_string()
                .starts_with("unsupported-agent:")
        );
        Ok(())
    }

    #[test]
    fn codex_native_contract_keeps_availability_and_verification_separate() -> anyhow::Result<()> {
        let value = inspect("codex")?;
        assert_eq!(value.get("availability"), Some(&json!("not-probed")));
        assert_eq!(value.get("verified_task_completion"), Some(&json!(false)));
        for operation in [
            "native-correlation",
            "working",
            "input-required",
            "response",
            "native-interrupt",
            "native-session-recreation",
        ] {
            assert_eq!(
                value.pointer(&format!("/capabilities/{operation}/level")),
                Some(&json!("native"))
            );
        }
        assert!(
            value
                .pointer("/capabilities/native-session-recreation/reason")
                .and_then(Value::as_str)
                .is_some_and(|reason| reason.contains("lost-wrapper restart is unsupported"))
        );
        Ok(())
    }
}
