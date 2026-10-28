//! The keys, frozen definitions and checked preferences owned by one client.
use crate::command::{
    DecodeMillis, EnvName, HistoryBytes, Limit, PictureBytes, Program, ScrollLines,
};
use crate::keymap::Keymap;
use crate::script::settings::Reads;
use crate::script::{Config, Fux};
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
    pub of: Option<crate::command::Run>,
    pub target: Option<crate::command::AttachTarget>,
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
            attach: self
                .attach
                .ok_or("bundled initialization did not set attach-defaults")?,
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
                        crate::words::listed(NAMES, "and"))),
                }
                Ok(())
            }

            /// Whether a setter ran, even if it repeated a bundled value.
            pub fn changed(&self) -> bool {
                self.changed
            }

            fn finish(self) -> Result<Preferences, String> {
                Ok(Preferences {
                    $($field: self.$field.ok_or_else(||
                        format!("bundled initialization did not set {}", $name))?,)*
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
    /// This client's limit for waits without an explicit timeout.
    wait "wait-seconds": Limit;
    /// Whether newly created panes may copy program output to paste buffers.
    program_clipboard "program-clipboard": bool;
    /// Whether copies are delivered to this client's terminal clipboard.
    terminal_clipboard "terminal-clipboard": bool;
    /// Whether this client rings and shows tab bell marks.
    bell "bell": bool;
    /// Whether this client's terminal title follows its focused pane.
    titles "titles": bool;
    /// Painter roles; changes may supply a subset of the initialized roles.
    styles "styles": crate::presentation::Styles;
    /// Position, ordered left and right fields, and tab numbering.
    bar "bar": crate::presentation::Bar;
    /// Inert text supplied explicitly by this client, never evaluated on paint.
    slots "bar-slots": crate::presentation::Slots;
    /// Whether the prefix column is drawn; dispatch and bar feedback stay active.
    show_column "show-column": bool;
    /// Names selected from this client for new launches. Their concrete
    /// values and unsets are saved for respawn and dump.
    copy_environment "copy-environment": Vec<EnvName>;
}

impl PreferenceDraft {
    pub(crate) fn pane_options(&self) -> Result<crate::command::PaneOptions, String> {
        Ok(crate::command::PaneOptions {
            reflow: self
                .reflow
                .ok_or("bundled initialization did not set reflow")?,
            alternate_scroll_lines: self
                .alternate_scroll_lines
                .ok_or("bundled initialization did not set alternate-scroll-lines")?,
            picture_bytes: self
                .picture_bytes
                .ok_or("bundled initialization did not set picture-bytes")?,
            history_bytes: self
                .history_bytes
                .ok_or("bundled initialization did not set history-bytes")?,
            program_clipboard: self
                .program_clipboard
                .ok_or("bundled initialization did not set program-clipboard")?,
        })
    }
    pub(crate) fn wait(&self) -> Result<Limit, String> {
        self.wait
            .ok_or_else(|| "bundled initialization did not set wait-seconds".to_owned())
    }
    /// Seed an in-process client draft from its own view.
    pub fn presentation(&mut self, p: crate::view::Presentation) {
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
        self.shell
            .as_ref()
            .ok_or_else(|| "bundled initialization did not set shell".to_owned())
    }
    pub(crate) fn copy_environment(&self) -> Result<&[EnvName], String> {
        self.copy_environment
            .as_deref()
            .ok_or_else(|| "bundled initialization did not set copy-environment".to_owned())
    }
}

impl Preferences {
    pub fn decode_timing(&self) -> crate::decode::Timing {
        crate::decode::Timing::new(
            u16::try_from(self.escape_delay.get()).unwrap_or(u16::MAX),
            u16::try_from(self.paste_timeout.get()).unwrap_or(u16::MAX),
        )
    }

    pub fn pane_options(&self) -> crate::command::PaneOptions {
        crate::command::PaneOptions {
            history_bytes: self.history_bytes,
            program_clipboard: self.program_clipboard,
            reflow: self.reflow,
            alternate_scroll_lines: self.alternate_scroll_lines,
            picture_bytes: self.picture_bytes,
        }
    }
    pub fn presentation(&self) -> crate::view::Presentation {
        crate::view::Presentation {
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

    #[test]
    fn initialization_requires_every_preference_and_the_prefix() -> Result<(), String> {
        assert!(Draft::default().finish().is_err());
        let state = crate::script::default_state().map_err(|e| e.to_string())?;
        let mut draft = state.draft();
        draft.preferences.wait = None;
        assert!(draft.finish().is_err());
        let mut draft = state.draft();
        draft.attach = None;
        assert!(draft.finish().is_err());
        assert_eq!(state.preferences.history_bytes.get(), 8 << 20);
        assert_eq!(state.preferences.wait.seconds(), 30);
        assert!(state.preferences.reflow);
        assert_eq!(state.preferences.alternate_scroll_lines.get(), 5);
        assert_eq!(state.preferences.picture_bytes.get(), fux_vt::PICTURE_BYTES);
        assert_eq!(state.preferences.escape_delay.get(), 35);
        assert_eq!(state.preferences.paste_timeout.get(), 5000);
        assert!(!state.preferences.cell_placement);
        assert!(state.preferences.program_clipboard && state.preferences.terminal_clipboard);
        assert!(state.preferences.bell && !state.preferences.titles);
        assert!(state.preferences.copy_environment.is_empty());
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
    fn local_settings_keep_the_checked_value_union_and_inner_validators() -> Result<(), String> {
        let list = |words: &[&str]| {
            fuxth::List::new(words.iter().map(|w| Value::str(*w)).collect())
                .map(|l| Value::List(std::rc::Rc::new(l)))
                .ok_or_else(|| "list".to_owned())
        };
        let mut draft = PreferenceDraft::default();
        for (name, value) in [
            ("history-bytes", Value::Int(0)),
            ("wait-seconds", Value::Int(0)),
            ("program-clipboard", Value::Bool(false)),
            ("terminal-clipboard", Value::Bool(false)),
            ("shell", list(&["/bin/my shell", "a b", ""])?),
            ("copy-environment", list(&["OK", "FUX_EXTRA"])?),
        ] {
            draft.set(name, &value)?;
        }
        for (name, value) in [
            ("reflow", Value::Int(1)),
            ("alternate-scroll-lines", Value::Int(-1)),
            ("alternate-scroll-lines", Value::Int(32768)),
            ("picture-bytes", Value::Int(100663297)),
            ("escape-delay-ms", Value::Int(-1)),
            ("paste-timeout-ms", Value::Int(65536)),
            ("cell-placement", Value::str("cells")),
            ("history-bytes", Value::Int(-1)),
            ("history-bytes", Value::Int((1 << 30) + 1)),
            ("wait-seconds", Value::Int(86_401)),
            ("wait-seconds", Value::Int(-1)),
            ("bell", Value::Int(0)),
            ("bell", Value::str("off")),
            ("shell", list(&[])?),
            ("shell", list(&[""])?),
            ("shell", list(&["/bin/sh", "a\0b"])?),
            ("shell", Value::str("/bin/sh")),
            ("copy-environment", list(&["BAD-NAME"])?),
            ("copy-environment", list(&["FUX_PANE"])?),
            ("copy-environment", list(&["FUX_SOCKET"])?),
            ("copy-environment", list(&["FUX_SERVER"])?),
            ("copy-environment", list(&["FUX"])?),
            ("buffers", Value::Int(0)),
            ("buffers", Value::Int(16)),
            ("clipboard", Value::Bool(true)),
        ] {
            assert!(draft.set(name, &value).is_err(), "{name}: {value:?}");
        }
        let tokens =
            fuxth::token::source("test", "str bell str off set").map_err(|e| e.to_string())?;
        assert!(crate::script::compile(&tokens).is_err());
        Ok(())
    }
}
