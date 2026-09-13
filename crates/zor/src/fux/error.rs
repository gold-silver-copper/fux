//! Failure provenance. Only validated remote envelopes expose retry-relevant codes.
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RemoteFailure {
    pub(super) code: String,
    message: String,
}
impl std::fmt::Display for RemoteFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for RemoteFailure {}

/// Manager discovery/creation/info refusals carry a message but no protocol error code.
#[derive(Debug)]
pub(super) struct Refused(pub(super) String);
impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "manager refused: {}", self.0)
    }
}
impl std::error::Error for Refused {}

#[derive(Debug)]
pub(super) struct MalformedReply(pub(super) anyhow::Error);
impl std::fmt::Display for MalformedReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "malformed fux reply: {}", self.0)
    }
}
impl std::error::Error for MalformedReply {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

#[derive(Debug)]
pub(super) enum TransportFailure {
    Io(std::io::Error),
    Deadline,
    Closed,
}
impl std::fmt::Display for TransportFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "fux transport: {error}"),
            Self::Deadline => f.write_str("fux request deadline exceeded"),
            Self::Closed => f.write_str("control socket closed before a response"),
        }
    }
}
impl std::error::Error for TransportFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

pub(crate) fn remote_code(error: &anyhow::Error) -> Option<&str> {
    error
        .downcast_ref::<RemoteFailure>()
        .map(|error| error.code.as_str())
}

/// No server was available; malformed envelopes and explicit refusals never qualify.
pub(crate) fn is_unavailable(error: &anyhow::Error) -> bool {
    matches!(error.downcast_ref::<TransportFailure>(), Some(TransportFailure::Io(error))
        if matches!(error.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused))
}

pub(super) fn malformed(error: impl Into<anyhow::Error>) -> anyhow::Error {
    MalformedReply(error.into()).into()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Transport,
    MalformedReply,
    RemoteFailure,
    Refused,
    StaleIdentity,
}
#[derive(Debug)]
struct StaleIdentity(&'static str);
impl std::fmt::Display for StaleIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for StaleIdentity {}

pub(crate) fn kind(error: &anyhow::Error) -> Option<Kind> {
    if error.is::<TransportFailure>() {
        Some(Kind::Transport)
    } else if error.is::<MalformedReply>() {
        Some(Kind::MalformedReply)
    } else if error.is::<RemoteFailure>() {
        Some(Kind::RemoteFailure)
    } else if error.is::<Refused>() {
        Some(Kind::Refused)
    } else if error.is::<StaleIdentity>() {
        Some(Kind::StaleIdentity)
    } else {
        None
    }
}

pub(super) fn stale(message: &'static str) -> anyhow::Error {
    StaleIdentity(message).into()
}

/// The decoder owns all schema and envelope validation. Preserve typed remote outcomes;
/// unclassified validation failures are malformed evidence, never remote failure codes.
pub(super) fn reply<T>(decode: impl FnOnce() -> anyhow::Result<T>) -> anyhow::Result<T> {
    decode().map_err(|error| {
        if kind(&error).is_some() {
            error
        } else {
            malformed(error)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Context, Result};

    #[test]
    fn failure_categories_survive_context_and_cannot_forge_remote_codes() -> Result<()> {
        let remote = RemoteFailure {
            code: "not-found".into(),
            message: "gone".into(),
        };
        let error = reply::<()>(|| Err(remote.into()))
            .err()
            .context("remote refusal")?
            .context("operation");
        assert_eq!(kind(&error), Some(Kind::RemoteFailure));
        assert_eq!(remote_code(&error), Some("not-found"));
        for (error, expected) in [
            (TransportFailure::Closed.into(), Kind::Transport),
            (TransportFailure::Deadline.into(), Kind::Transport),
            (Refused("exists".into()).into(), Kind::Refused),
            (stale("replacement"), Kind::StaleIdentity),
            (
                anyhow::anyhow!("not-found: fabricated refusal"),
                Kind::MalformedReply,
            ),
        ] {
            let classified = reply::<()>(|| Err(error))
                .err()
                .context("expected failure")?
                .context("caller");
            assert_eq!(kind(&classified), Some(expected));
            assert_eq!(remote_code(&classified), None);
            assert!(!is_unavailable(&classified));
        }
        let missing = anyhow::Error::from(TransportFailure::Io(std::io::Error::from(
            std::io::ErrorKind::NotFound,
        )))
        .context("connect");
        assert!(is_unavailable(&missing));
        assert_eq!(kind(&missing), Some(Kind::Transport));
        Ok(())
    }
}
