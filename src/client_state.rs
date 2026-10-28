//! The keys, frozen definitions and checked preferences owned by one client.
use crate::config::{EnvName, Limit, Number, Program, Reads};
use crate::keymap::Keymap;
use crate::script::{Config, Fux};
use fuxth::Value;

/// A complete client state. A program changes a draft and commits it whole.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientState {
    pub keys: Keymap,
    pub config: Option<Config>,
    pub preferences: Preferences,
}

impl ClientState {
    pub fn draft(&self) -> Draft {
        Draft {
            keys: self.keys.clone(),
            config: self.config.clone(),
            preferences: self.preferences.clone().into(),
        }
    }
}

/// Initialization may leave fields missing; a live client may not.
#[derive(Clone, Debug, Default)]
pub struct Draft {
    pub keys: Keymap,
    pub config: Option<Config>,
    pub preferences: PreferenceDraft,
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
        })
    }
}

/// The closed local table keeps each setting's existing checked reader.
macro_rules! preferences {
    ($($(#[doc = $about:literal])* $field:ident $name:literal: $reads:ty $(=> $variant:ident)?;)*) => {
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub struct Preferences {
            $($(#[doc = $about])* pub $field: $reads,)*
        }

        /// Fields explicitly written in one local transaction.
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        pub struct Changes {
            $($field: bool,)*
        }

        impl Changes {
            pub fn any(self) -> bool { false $(|| self.$field)* }
            pub fn merge(self, other: Self) -> Self {
                Self { $($field: self.$field || other.$field,)* }
            }
            fn settings(self, p: &Preferences) -> Vec<crate::config::Setting> {
                let mut settings = Vec::new();
                $($(if self.$field {
                    settings.push(crate::config::Setting::$variant(p.$field.clone()));
                })?)*
                settings
            }
        }

        #[derive(Clone, Debug, Default)]
        pub struct PreferenceDraft {
            $($field: Option<$reads>,)*
            changes: Changes,
        }

        impl PreferenceDraft {
            pub fn set(&mut self, name: &str, value: &Value<Fux>) -> Result<(), String> {
                match name {
                    $($name => {
                        self.$field = Some(<$reads as Reads>::read(value)
                            .map_err(|why| format!("{name}: {why}"))?);
                        self.changes.$field = true;
                    },)*
                    other => return Err(format!("no setting {other:?}; the settings are {}",
                        crate::words::listed(NAMES, "and"))),
                }
                Ok(())
            }

            /// Whether a setter ran, even if it repeated a bundled value.
            pub fn changed(&self) -> bool {
                self.changes.any()
            }

            pub fn changes(&self) -> Changes {
                self.changes
            }

            pub(crate) fn include(&mut self, changes: Changes) {
                self.changes = self.changes.merge(changes);
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
                Self { $($field: Some(p.$field),)* changes: Changes::default() }
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
    history_bytes "history-bytes": crate::config::HistoryBytes;
    /// The shared limit for waits without an explicit timeout.
    wait "wait-seconds": Limit => Wait;
    /// Whether newly created panes may copy program output to paste buffers.
    program_clipboard "program-clipboard": bool;
    /// Whether copies are delivered to this client's terminal clipboard.
    terminal_clipboard "terminal-clipboard": bool;
    /// The number of shared paste buffers retained.
    buffers "buffers": Number<1, { crate::config::MAX_BUFFERS }> => Buffers;
    /// Whether this client rings and shows tab bell marks.
    bell "bell": bool;
    /// Whether this client's terminal title follows its focused pane.
    titles "titles": bool;
    /// Names selected from this client for new launches. Their concrete
    /// values and unsets are saved for respawn and dump.
    copy_environment "copy-environment": Vec<EnvName>;
}

impl PreferenceDraft {
    pub(crate) fn pane_options(&self) -> Result<crate::command::PaneOptions, String> {
        Ok(crate::command::PaneOptions {
            history_bytes: self
                .history_bytes
                .ok_or("bundled initialization did not set history-bytes")?,
            program_clipboard: self
                .program_clipboard
                .ok_or("bundled initialization did not set program-clipboard")?,
        })
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
    pub fn pane_options(&self) -> crate::command::PaneOptions {
        crate::command::PaneOptions {
            history_bytes: self.history_bytes,
            program_clipboard: self.program_clipboard,
        }
    }
    pub fn presentation(&self) -> crate::view::Presentation {
        crate::view::Presentation {
            bell: self.bell,
            titles: self.titles,
            terminal_clipboard: self.terminal_clipboard,
        }
    }
    /// Temporary bridge for consumers still owned by the server. Call only
    /// after committing local state; setters and config evaluation never send.
    pub fn forward_settings<A: crate::script::Asks + ?Sized>(
        &self,
        changes: Changes,
        asks: &mut A,
        context: crate::protocol::Context,
    ) -> Result<(), crate::script::Failed> {
        let settings = changes.settings(self);
        if settings.is_empty() {
            return Ok(());
        }
        let reply = asks.ask(
            &crate::protocol::Asked {
                request: crate::command::Command::Set(settings).into(),
                context,
                after: None,
            },
            &mut std::io::sink(),
        )?;
        crate::script::succeeded(reply).map(drop)
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
        assert_eq!(state.preferences.history_bytes.get(), 8 << 20);
        assert_eq!(state.preferences.wait.seconds(), 30);
        assert_eq!(state.preferences.buffers.get(), 16);
        assert!(state.preferences.program_clipboard && state.preferences.terminal_clipboard);
        assert!(state.preferences.bell && !state.preferences.titles);
        assert!(state.preferences.copy_environment.is_empty());
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
            ("buffers", Value::Int(1001)),
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
