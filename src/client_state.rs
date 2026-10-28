//! The keys, frozen definitions and checked preferences owned by one client.
use crate::config::{EnvName, Limit, Number, Reads, Shell};
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
    ($($(#[doc = $about:literal])* $field:ident $name:literal: $reads:ty;)*) => {
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub struct Preferences {
            $($(#[doc = $about])* pub $field: $reads,)*
        }

        #[derive(Clone, Debug, Default)]
        pub struct PreferenceDraft {
            $($field: Option<$reads>,)*
        }

        impl PreferenceDraft {
            pub fn set(&mut self, name: &str, value: &Value<Fux>) -> Result<(), String> {
                match name {
                    $($name => self.$field = Some(<$reads as Reads>::read(value)
                        .map_err(|why| format!("{name}: {why}"))?),)*
                    other => return Err(format!("no setting {other:?}; the settings are {}",
                        crate::words::listed(NAMES, "and"))),
                }
                Ok(())
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
                Self { $($field: Some(p.$field),)* }
            }
        }

        pub const NAMES: &[&str] = &[$($name),*];

        /// Help describes policy's source without evaluating defaults.
        pub fn table() -> Vec<[String; 4]> {
            vec![$([
                $name.to_owned(),
                <$reads as Reads>::takes(),
                "client-defaults.fux".to_owned(),
                [$($about.trim()),*].join(" "),
            ],)*]
        }
    };
}

preferences! {
    /// The shell argv for this client's future panes.
    shell "shell": Shell;
    /// The history budget for this client's future panes, in bytes.
    history_bytes "history-bytes": Number<0, { 1 << 30 }>;
    /// The limit for this client's waits without an explicit timeout.
    wait "wait-seconds": Limit;
    /// Whether programs in this client's future panes may write copies.
    program_clipboard "program-clipboard": bool;
    /// Whether copies may reach this client's terminal clipboard.
    terminal_clipboard "terminal-clipboard": bool;
    /// Whether this client rings and shows tab bell marks.
    bell "bell": bool;
    /// Whether this client's terminal title follows the focused pane.
    titles "titles": bool;
    /// Environment names to select for this client's future launches.
    copy_environment "copy-environment": Vec<EnvName>;
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
            ("buffers", Value::Int(16)),
            ("clipboard", Value::Bool(true)),
            ("update-environment", list(&[])?),
        ] {
            assert!(draft.set(name, &value).is_err(), "{name}: {value:?}");
        }
        let tokens =
            fuxth::token::source("test", "str bell str off set").map_err(|e| e.to_string())?;
        assert!(crate::script::compile(&tokens).is_err());
        Ok(())
    }
}
