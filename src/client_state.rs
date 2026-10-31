//! The keys, frozen definitions and checked preferences owned by one client.
use crate::keymap::Keymap;
use crate::script::settings::Reads;
use crate::script::{Config, Fux};
use fux_wire::command::{
    DecodeMillis, HistoryBytes, JournalEntries, PictureBytes, Program, ScrollLines,
};
use fuxth::Value;

/// A complete client state. A program changes a draft and commits it whole.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientState {
    pub keys: Keymap,
    pub config: Option<Config>,
    pub preferences: Preferences,
    pub attach: AttachDefaults,
}

impl ClientState {
    pub fn draft(&self) -> Draft {
        Draft {
            keys: self.keys.clone(),
            config: self.config.clone(),
            preferences: self.preferences.clone().into(),
            attach: Some(self.attach.clone()),
        }
    }
}

/// A preferred place and the name used only for the first workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachDefaults {
    pub of: Option<fux_wire::command::Run>,
    pub target: Option<fux_wire::command::AttachTarget>,
    pub name: Option<String>,
}

/// Initialization may leave fields missing; a live client may not.
#[derive(Clone, Debug, Default)]
pub struct Draft {
    pub keys: Keymap,
    pub config: Option<Config>,
    pub preferences: PreferenceDraft,
    pub attach: Option<AttachDefaults>,
}

impl Draft {
    pub fn initialized(&self) -> bool {
        self.keys.prefix.is_some()
    }

    pub fn finish(self) -> Result<ClientState, String> {
        if self.keys.prefix.is_none() {
            return Err("bundled initialization did not set the prefix".to_owned());
        }
        Ok(ClientState {
            keys: self.keys,
            config: self.config,
            preferences: self.preferences.finish()?,
            attach: required(self.attach, "attach-defaults")?,
        })
    }
}

/// The closed local table keeps each setting's existing checked reader.
macro_rules! preferences {
    ($($(#[doc = $about:literal])* $field:ident $name:literal: $reads:ty;)*) => {
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub struct Preferences {
            $($(#[doc = $about])* pub $field: $reads,)*
        }

        #[derive(Clone, Debug, Default)]
        pub struct PreferenceDraft {
            $($field: Option<$reads>,)*
            changed: bool,
        }

        impl PreferenceDraft {
            pub fn set(&mut self, name: &str, value: &Value<Fux>) -> Result<(), String> {
                match name {
                    $($name => {
                        self.$field = Some(<$reads as Reads>::update(value, self.$field.as_ref())
                            .map_err(|why| format!("{name}: {why}"))?);
                        self.changed = true;
                    },)*
                    other => return Err(format!("no setting {other:?}; the settings are {}",
                        fux_wire::words::listed(NAMES, "and"))),
                }
                Ok(())
            }

            /// Whether a setter ran, even if it repeated a bundled value.
            pub fn changed(&self) -> bool {
                self.changed
            }

            fn finish(self) -> Result<Preferences, String> {
                Ok(Preferences {
                    $($field: required(self.$field, $name)?,)*
                })
            }
        }

        impl From<Preferences> for PreferenceDraft {
            fn from(p: Preferences) -> Self {
                Self { $($field: Some(p.$field),)* changed: false }
            }
        }

        pub const NAMES: &[&str] = &[$($name),*];

        /// Help describes policy's source without evaluating defaults.
        pub fn table() -> Vec<[String; 3]> {
            vec![$([
                $name.to_owned(),
                <$reads as Reads>::takes(),
                [$($about.trim()),*].join(" "),
            ],)*]
        }
    };
}

preferences! {
    /// This client's shell argv for new panes.
    shell "shell": Program;
    /// The history budget in bytes for panes this client creates.
    history_bytes "history-bytes": HistoryBytes;
    /// The events the journal of a server this client starts keeps.
    journal_entries "journal-entries": JournalEntries;
    /// Rewrap recorded primary-screen lines when a new pane is resized.
    reflow "reflow": bool;
    /// Arrows per alternate-screen wheel action; Ctrl keeps half-page scrolling.
    alternate_scroll_lines "alternate-scroll-lines": ScrollLines;
    /// Picture byte budget for new panes; zero disables picture support.
    picture_bytes "picture-bytes": PictureBytes;
    /// Legacy Escape wait, applied when this client's decoder is idle.
    escape_delay "escape-delay-ms": DecodeMillis;
    /// Unfinished paste wait, applied when this client's decoder is idle.
    paste_timeout "paste-timeout-ms": DecodeMillis;
    /// Position after each non-ASCII glyph; bounds drift, not glyph footprints.
    cell_placement "cell-placement": bool;
    /// Whether newly created panes' programs may copy to the terminals' clipboards (OSC 52).
    program_clipboard "program-clipboard": bool;
    /// Whether copies are delivered to this client's terminal clipboard.
    terminal_clipboard "terminal-clipboard": bool;
    /// Whether this client rings and shows tab bell marks.
    bell "bell": bool;
    /// Whether this client's terminal title follows its focused pane.
    titles "titles": bool;
    /// Painter roles; changes may supply a subset of the initialized roles.
    styles "styles": fux_wire::presentation::Styles;
    /// Position, ordered left and right fields, and tab numbering.
    bar "bar": fux_wire::presentation::Bar;
    /// Inert text supplied explicitly by this client, never evaluated on paint.
    slots "bar-slots": fux_wire::presentation::Slots;
    /// Whether the prefix column is drawn; dispatch and bar feedback stay active.
    show_column "show-column": bool;
}

/// An initialized preference, or the bundled setting that is missing.
fn required<T>(value: Option<T>, name: &str) -> Result<T, String> {
    value.ok_or_else(|| format!("bundled initialization did not set {name}"))
}

impl PreferenceDraft {
    pub(crate) fn pane_options(&self) -> Result<fux_wire::command::PaneOptions, String> {
        Ok(fux_wire::command::PaneOptions {
            reflow: required(self.reflow, "reflow")?,
            alternate_scroll_lines: required(
                self.alternate_scroll_lines,
                "alternate-scroll-lines",
            )?,
            picture_bytes: required(self.picture_bytes, "picture-bytes")?,
            history_bytes: required(self.history_bytes, "history-bytes")?,
            program_clipboard: required(self.program_clipboard, "program-clipboard")?,
        })
    }
    /// Seed an in-process client draft from its own view.
    pub fn presentation(&mut self, p: fux_wire::presentation::Presentation) {
        self.styles = Some(p.styles);
        self.bar = Some(p.bar);
        self.slots = Some(p.slots);
        self.show_column = Some(p.show_column);
        self.cell_placement = Some(p.cell_placement);
        self.bell = Some(p.bell);
        self.titles = Some(p.titles);
        self.terminal_clipboard = Some(p.terminal_clipboard);
    }
    pub(crate) fn shell(&self) -> Result<&Program, String> {
        required(self.shell.as_ref(), "shell")
    }
    pub(crate) fn journal_entries(&self) -> Result<JournalEntries, String> {
        required(self.journal_entries, "journal-entries")
    }
}

impl Preferences {
    pub fn decode_timing(&self) -> fux_vt::keys::decode::Timing {
        fux_vt::keys::decode::Timing::new(
            u16::try_from(self.escape_delay.get()).unwrap_or(u16::MAX),
            u16::try_from(self.paste_timeout.get()).unwrap_or(u16::MAX),
        )
    }

    pub fn pane_options(&self) -> fux_wire::command::PaneOptions {
        fux_wire::command::PaneOptions {
            history_bytes: self.history_bytes,
            program_clipboard: self.program_clipboard,
            reflow: self.reflow,
            alternate_scroll_lines: self.alternate_scroll_lines,
            picture_bytes: self.picture_bytes,
        }
    }
    pub fn presentation(&self) -> fux_wire::presentation::Presentation {
        fux_wire::presentation::Presentation {
            styles: self.styles,
            bar: self.bar.clone(),
            slots: self.slots.clone(),
            show_column: self.show_column,
            cell_placement: self.cell_placement,
            bell: self.bell,
            titles: self.titles,
            terminal_clipboard: self.terminal_clipboard,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TestResult;
    use std::rc::Rc;

    #[test]
    fn initialization_requires_every_preference_and_the_prefix() -> TestResult {
        assert!(Draft::default().finish().is_err());
        let state = crate::script::default_state()?;
        let mut draft = state.draft();
        draft.attach = None;
        assert!(draft.finish().is_err());
        assert_eq!(state.preferences.history_bytes.get(), 8 << 20);
        assert_eq!(state.preferences.journal_entries.get(), 16_384);
        assert!(state.preferences.reflow);
        assert_eq!(state.preferences.alternate_scroll_lines.get(), 5);
        assert_eq!(state.preferences.picture_bytes.get(), fux_vt::PICTURE_BYTES);
        assert_eq!(state.preferences.escape_delay.get(), 35);
        assert_eq!(state.preferences.paste_timeout.get(), 5000);
        assert!(!state.preferences.cell_placement);
        assert!(state.preferences.program_clipboard && state.preferences.terminal_clipboard);
        assert!(state.preferences.bell && !state.preferences.titles);
        assert!(state.preferences.show_column);
        let mut draft = state.draft();
        draft.preferences.styles = None;
        assert!(draft.finish().is_err());
        let mut draft = state.draft();
        draft.preferences.bar = None;
        assert!(draft.finish().is_err());
        Ok(())
    }

    #[test]
    fn local_settings_keep_the_checked_value_union_and_inner_validators() -> TestResult {
        let list = |words: &[&str]| {
            let list = fuxth::List::new(words.iter().map(|w| Value::str(*w)).collect());
            Ok::<_, String>(Value::List(Rc::new(list)))
        };
        let mut draft = PreferenceDraft::default();
        for (name, value) in [
            ("history-bytes", Value::Int(0)),
            ("program-clipboard", Value::Bool(false)),
            ("show-column", Value::Bool(false)),
            ("terminal-clipboard", Value::Bool(false)),
            ("shell", list(&["/bin/my shell", "a b", ""])?),
        ] {
            draft.set(name, &value)?;
        }
        for (name, value) in [
            ("reflow", Value::Int(1)),
            ("show-column", Value::Int(1)),
            ("alternate-scroll-lines", Value::Int(-1)),
            ("alternate-scroll-lines", Value::Int(32768)),
            ("picture-bytes", Value::Int(100663297)),
            ("escape-delay-ms", Value::Int(-1)),
            ("paste-timeout-ms", Value::Int(65536)),
            ("cell-placement", Value::str("cells")),
            ("history-bytes", Value::Int(-1)),
            ("history-bytes", Value::Int((1 << 30) + 1)),
            ("journal-entries", Value::Int(0)),
            ("bell", Value::Int(0)),
            ("bell", Value::str("off")),
            ("shell", list(&[])?),
            ("shell", list(&[""])?),
            ("shell", list(&["/bin/sh", "a\0b"])?),
            ("shell", Value::str("/bin/sh")),
            ("buffers", Value::Int(0)),
            ("buffers", Value::Int(16)),
            ("clipboard", Value::Bool(true)),
        ] {
            assert!(draft.set(name, &value).is_err(), "{name}: {value:?}");
        }
        let tokens = fuxth::token::source("test", "str bell str off set")?;
        assert!(crate::script::compile(&tokens).is_err());
        let tokens = fuxth::token::source("test", "false show-column")?;
        assert!(crate::script::compile(&tokens).is_err());
        Ok(())
    }
}
