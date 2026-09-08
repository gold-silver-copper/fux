//! Fixture-only contention retries. Transport/semantic failures never authorize replay.
use anyhow::{Result, ensure};
use serde_json::Value;
use std::time::{Duration, Instant};

pub const BUSY_STDERR: &[u8] = b"zor: zor journal is busy; retry the operation ID\n";

pub fn cli_busy(code: Option<i32>, stderr: &[u8]) -> bool {
    code == Some(1) && stderr == BUSY_STDERR
}
pub fn api_busy(reply: &Value, request: &Value) -> Result<bool> {
    if reply["error"] != "task-busy" {
        return Ok(false);
    }
    ensure!(
        request["op"] == "task",
        "Busy envelope requires a task request"
    );
    ensure!(
        reply["v"] == 1 && reply["id"] == request["id"] && !request["id"].is_null(),
        "Busy envelope version/request mismatch"
    );
    ensure!(
        reply["status"] == "failed",
        "Busy envelope is not a failure"
    );
    ensure!(
        reply["service_instance"] == request["service_instance"]
            && !request["service_instance"].is_null(),
        "Busy service incarnation mismatch"
    );
    Ok(true)
}

pub trait Clock {
    fn now(&self) -> Instant;
    fn sleep(&self, duration: Duration);
}
pub struct RealClock;
impl Clock for RealClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// Explicit replay admission is mandatory; a Busy result does not imply idempotency.
pub fn retry_busy<T>(
    mut call: impl FnMut(Duration) -> Result<T>,
    mut is_busy: impl FnMut(&T) -> Result<bool>,
    deadline: Instant,
    replay: bool,
    clock: &impl Clock,
) -> Result<T> {
    loop {
        let remaining = deadline
            .checked_duration_since(clock.now())
            .filter(|left| !left.is_zero())
            .ok_or_else(|| anyhow::anyhow!("journal contention deadline"))?;
        let reply = call(remaining)?;
        if !is_busy(&reply)? {
            return Ok(reply);
        }
        ensure!(replay, "operation requires reconciliation after Busy");
        let remaining = deadline
            .checked_duration_since(clock.now())
            .filter(|left| !left.is_zero())
            .ok_or_else(|| anyhow::anyhow!("journal contention deadline"))?;
        clock.sleep(Duration::from_millis(30).min(remaining));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::Cell;

    struct FakeClock(Cell<Instant>);
    impl Clock for FakeClock {
        fn now(&self) -> Instant {
            self.0.get()
        }
        fn sleep(&self, duration: Duration) {
            self.0.set(self.0.get() + duration);
        }
    }
    #[test]
    fn cli_busy_requires_exact_exit_and_stderr() {
        for (code, stderr, expected) in [
            (1, BUSY_STDERR.to_vec(), true),
            (0, BUSY_STDERR.to_vec(), false),
            (2, BUSY_STDERR.to_vec(), false),
            (1, [b"prefix ".as_slice(), BUSY_STDERR].concat(), false),
            (1, [BUSY_STDERR, b"other failure"].concat(), false),
        ] {
            assert_eq!(cli_busy(Some(code), &stderr), expected);
        }
    }
    #[test]
    fn api_busy_checks_envelope_and_incarnation() -> Result<()> {
        let request = json!({"v":1,"id":8,"op":"task","service_instance":"original"});
        let reply = json!({"v":1,"id":8,"status":"failed","service_instance":"original","error":"task-busy"});
        assert!(api_busy(&reply, &request)?);
        for (key, value) in [
            ("v", json!(2)),
            ("id", json!(9)),
            ("status", json!("completed")),
            ("service_instance", json!("replacement")),
        ] {
            let mut wrong = reply.clone();
            wrong[key] = value;
            assert!(api_busy(&wrong, &request).is_err());
        }
        let mut other = reply;
        other["error"] = json!("task-failed");
        assert!(!api_busy(&other, &request)?);
        Ok(())
    }
    #[test]
    fn contention_cannot_satisfy_an_expected_semantic_rejection() -> Result<()> {
        let clock = FakeClock(Cell::new(Instant::now()));
        let mut replies = ["busy", "actual rejection"].into_iter();
        assert_eq!(
            retry_busy(
                |_| Ok(replies.next().unwrap()),
                |r| Ok(*r == "busy"),
                clock.now() + Duration::from_secs(1),
                true,
                &clock
            )?,
            "actual rejection"
        );
        Ok(())
    }
    #[test]
    fn non_idempotent_effect_then_busy_cannot_replay() {
        let clock = FakeClock(Cell::new(Instant::now()));
        let mut effects = Vec::new();
        let result = retry_busy(
            |_| {
                effects.push(if effects.is_empty() { "alpha" } else { "beta" });
                Ok("busy")
            },
            |_| Ok(true),
            clock.now() + Duration::from_secs(1),
            false,
            &clock,
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("requires reconciliation")
        );
        assert_eq!(effects, ["alpha"]);
    }
    #[test]
    fn original_deadline_survives_busy_retries() {
        let clock = FakeClock(Cell::new(Instant::now()));
        let mut budgets = Vec::new();
        let result = retry_busy(
            |remaining| {
                budgets.push(remaining);
                clock.sleep(Duration::from_millis(400));
                Ok("busy")
            },
            |_| Ok(true),
            clock.now() + Duration::from_secs(1),
            true,
            &clock,
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("contention deadline")
        );
        assert_eq!(budgets.len(), 3);
        assert!(budgets[0] > budgets[1] && budgets[1] > budgets[2]);
    }
    #[test]
    fn expired_deadline_never_calls_and_transport_errors_never_retry() {
        let clock = FakeClock(Cell::new(Instant::now()));
        let mut calls = 0;
        let result = retry_busy(
            |_| {
                calls += 1;
                Ok(())
            },
            |_| Ok(false),
            clock.now(),
            true,
            &clock,
        );
        assert!(result.is_err());
        assert_eq!(calls, 0);
        let result = retry_busy(
            |_| -> Result<()> {
                calls += 1;
                anyhow::bail!("reply lost after effect")
            },
            |_| Ok(true),
            clock.now() + Duration::from_secs(1),
            true,
            &clock,
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("reply lost after effect")
        );
        assert_eq!(calls, 1);
    }
}
