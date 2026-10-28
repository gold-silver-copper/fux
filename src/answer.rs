//! What a reply answers of panes, and of everything there is, as data the
//! client renders as text or as JSON (`json`), each from the one value.
use crate::code::Code;
use crate::command::Version;
use crate::id::PaneId;
use crate::json::Json;
use crate::process::Ended;
use crate::wire::{closed_wire, wire, wire_enum};

wire! {
    /// What one pane answers, of an action or a wait on it.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum PaneAnswer {
        /// An action's (keys, a mouse event, a signal, a program started):
        /// its pane's version before it acted, or as it starts, which a
        /// wait for its effect waits to differ from.
        Acted(version: Version),
        /// A wait's, its pane changed: its version now, what changed, and
        /// how its program ended, if it ended since.
        Changed {
            version: Version,
            changed: Vec<Change>,
            ended: Option<Ended>,
        },
        /// A wait's, its pane closed.
        Closed(pane: PaneId),
        /// A wait's, nothing changed in time: the version it is still at.
        TimedOut { since: Version, limit: std::time::Duration },
    }
}

wire! {
    /// One pane's answer, or all the members that answered, in watch order.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum PaneResults {
        One(answer: PaneAnswer),
        Many(answers: Vec<PaneAnswer>),
    }
}

impl PaneResults {
    pub fn all(&self) -> &[PaneAnswer] {
        match self {
            Self::One(one) => std::slice::from_ref(one),
            Self::Many(all) => all,
        }
    }
}

/// What changed of a pane a wait answers: its program started again
/// (whose other counts say nothing of the last one's), or its screen, a
/// bell, a shell command's end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Respawned,
    Screen,
    Bell,
    Done,
}

impl Change {
    pub const ALL: [Change; 4] = [
        Change::Respawned,
        Change::Screen,
        Change::Bell,
        Change::Done,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Change::Respawned => "respawned",
            Change::Screen => "screen",
            Change::Bell => "bell",
            Change::Done => "done",
        }
    }
}

closed_wire!(Change, "change");
wire_enum!(Ended { Exited(code), Signalled(signal), Lost });

impl PaneAnswer {
    pub fn pane(&self) -> PaneId {
        match self {
            PaneAnswer::Acted(version)
            | PaneAnswer::Changed { version, .. }
            | PaneAnswer::TimedOut { since: version, .. } => version.pane,
            PaneAnswer::Closed(pane) => *pane,
        }
    }

    /// The version it holds, which a wait on it waits from: none once its
    /// pane closed.
    pub fn version(&self) -> Option<Version> {
        match self {
            PaneAnswer::Acted(version)
            | PaneAnswer::Changed { version, .. }
            | PaneAnswer::TimedOut { since: version, .. } => Some(*version),
            PaneAnswer::Closed(_) => None,
        }
    }

    /// The codes of the failures one may be (`failure`), which carry it
    /// to a `try`'s handler.
    pub const FAILURES: [Code; 2] = [Code::Closed, Code::Timeout];

    /// The code of the failure it is, if it is one: one of `FAILURES`.
    pub fn failure(&self) -> Option<Code> {
        match self {
            PaneAnswer::Acted(_) | PaneAnswer::Changed { .. } => None,
            PaneAnswer::Closed(_) => Some(Code::Closed),
            PaneAnswer::TimedOut { .. } => Some(Code::Timeout),
        }
    }

    /// Why this outcome failed, if it did.
    pub fn why(&self) -> Option<String> {
        Some(match self {
            PaneAnswer::Acted(_) | PaneAnswer::Changed { .. } => return None,
            PaneAnswer::Closed(id) => format!("{id}: the pane closed"),
            PaneAnswer::TimedOut { since, limit } => format!(
                "{}: unchanged for {}s, still at version {since}",
                since.pane,
                limit.as_secs_f64()
            ),
        })
    }

    /// A line that names the pane: `%2:0.57.1.3.0:RUN screen done exited
    /// 0`, `%2 closed`, `%2:0.57.1.3.0:RUN timeout`.
    pub fn text(&self) -> String {
        struct Text<'a>(&'a PaneAnswer);
        impl std::fmt::Display for Text<'_> {
            fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.write_text(out)
            }
        }
        Text(self).to_string()
    }

    pub(crate) fn write_text(&self, out: &mut dyn std::fmt::Write) -> std::fmt::Result {
        match self {
            PaneAnswer::Acted(version) => write!(out, "{version}"),
            PaneAnswer::Changed {
                version,
                changed,
                ended,
            } => {
                write!(out, "{version}")?;
                for change in changed {
                    write!(out, " {}", change.name())?;
                }
                if let Some(ended) = ended {
                    write!(out, " {}", crate::process::Told(*ended))?;
                }
                Ok(())
            }
            PaneAnswer::Closed(pane) => write!(out, "{pane} closed"),
            PaneAnswer::TimedOut { since, .. } => write!(out, "{since} timeout"),
        }
    }

    pub(crate) fn charge(
        &self,
        budget: &mut crate::wire::Budget,
    ) -> Result<(), crate::session::Error> {
        if let PaneAnswer::Changed { changed, .. } = self {
            budget.slots::<Change>(changed.capacity())?;
        }
        Ok(())
    }

    /// As JSON, without allocating a second owned graph.
    pub(crate) fn write_json(&self, out: &mut dyn std::fmt::Write) -> std::fmt::Result {
        write!(out, "{{\"pane\":\"{}\"", self.pane())?;
        if let Some(version) = self.version() {
            write!(out, ",\"version\":\"{version}\"")?;
        }
        if let PaneAnswer::Changed { changed, ended, .. } = self {
            out.write_str(",\"changed\":[")?;
            for (i, change) in changed.iter().enumerate() {
                if i > 0 {
                    out.write_char(',')?;
                }
                crate::json::write_str(out, change.name())?;
            }
            out.write_char(']')?;
            match ended {
                Some(Ended::Exited(code)) => write!(out, ",\"exited\":{code}")?,
                Some(Ended::Signalled(signal)) => write!(out, ",\"signal\":{signal}")?,
                Some(Ended::Lost) => out.write_str(",\"exited\":null")?,
                None => {}
            }
        }
        write!(
            out,
            ",\"status\":{}}}",
            self.failure().map_or(0, |code| code.status().get())
        )
    }
}

/// A field as listings and status show it: string contents, other JSON,
/// or the caller's placeholder. No owned formatting temporary.
#[derive(Clone, Copy)]
pub(crate) struct Field<'a>(pub Option<&'a Json>, pub &'static str);
impl Field<'_> {
    fn plain(&self) -> Option<&str> {
        match self.0 {
            Some(Json::String(text)) => Some(text),
            None => Some(self.1),
            Some(_) => None,
        }
    }
}
impl std::fmt::Display for Field<'_> {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(Json::String(text)) => out.write_str(text),
            Some(json) => json.write(out),
            None => out.write_str(self.1),
        }
    }
}
impl PartialEq for Field<'_> {
    fn eq(&self, other: &Self) -> bool {
        fn same_json(a: &Json, b: &Json) -> bool {
            match (a, b) {
                (Json::Int(a), Json::Uint(b)) | (Json::Uint(b), Json::Int(a)) => {
                    u64::try_from(*a).ok() == Some(*b)
                }
                (Json::Array(a), Json::Array(b)) => {
                    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_json(a, b))
                }
                (Json::Object(a), Json::Object(b)) => {
                    a.len() == b.len()
                        && a.iter()
                            .zip(b)
                            .all(|((ka, a), (kb, b))| ka == kb && same_json(a, b))
                }
                _ => a == b,
            }
        }
        let same_plain = |plain: &str, json: Option<&Json>| {
            json.is_some_and(|json| crate::words::writes_exactly(plain, |out| json.write(out)))
        };
        match (self.plain(), other.plain()) {
            (Some(a), Some(b)) => a == b,
            (Some(a), None) => same_plain(a, other.0),
            (None, Some(b)) => same_plain(b, self.0),
            (None, None) => match (self.0, other.0) {
                (Some(a), Some(b)) => same_json(a, b),
                _ => false,
            },
        }
    }
}
