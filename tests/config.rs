#![allow(dead_code, clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "../src/config.rs"]
mod config;

use config::{
    Binding, BuiltinAction, ClipboardPolicy, Config, ConfigError, default_path_from,
    default_shell_from,
};
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

#[test]
fn xdg_path_wins_and_home_supplies_the_documented_fallback() {
    // Phase F5 configuration: `$XDG_CONFIG_HOME/fux/config.toml`, then `$HOME/.config`.
    let xdg = default_path_from(
        Some(OsString::from("/xdg")),
        Some(OsString::from("/home/me")),
    );
    assert_eq!(
        xdg.expect("XDG path"),
        PathBuf::from("/xdg/fux/config.toml")
    );
    let home = default_path_from(None, Some(OsString::from("/home/me")));
    assert_eq!(
        home.expect("HOME path"),
        PathBuf::from("/home/me/.config/fux/config.toml")
    );
    assert!(matches!(
        default_path_from(None, None),
        Err(ConfigError::NoConfigHome)
    ));
    assert!(matches!(
        default_path_from(Some(OsString::from("relative")), None),
        Err(ConfigError::NoConfigHome)
    ));
}

#[test]
fn android_and_termux_shell_fallbacks_do_not_assume_bin_sh() {
    assert_eq!(
        default_shell_from(
            None,
            Some(OsString::from("/data/data/com.termux/files/usr")),
            true
        ),
        "/data/data/com.termux/files/usr/bin/sh"
    );
    assert_eq!(default_shell_from(None, None, true), "/system/bin/sh");
}

#[test]
fn sparse_documents_merge_defaults_bindings_and_replace_lists() {
    // Phase F5 configuration: sparse files merge over defaults without retaining configured lists.
    let base = Config::from_toml(
        r#"
remote-allow-ids = ["old"]
hooks = [{ name = "old", command = { argv = ["old-hook"] } }]
[bindings.q]
builtin = "help"
"#,
    )
    .expect("base config");
    let merged = base
        .merge_toml(
            r#"
prefix = "C-b"
clipboard = "read-write"
remote-allow-ids = ["new"]
hooks = []
[bindings."|"]
builtin = "zoom"
[bindings."C-x"]
external = { argv = ["fux-helper", "--pick"] }
"#,
        )
        .expect("merged config");
    assert_eq!(merged.prefix, "C-b");
    assert_eq!(merged.clipboard, ClipboardPolicy::ReadWrite);
    assert_eq!(merged.remote_allow_ids, ["new"]);
    assert!(merged.hooks.is_empty());
    assert_eq!(
        merged.bindings.get("|"),
        Some(&Binding::Builtin {
            builtin: BuiltinAction::Zoom
        })
    );
    assert!(merged.bindings.contains_key("q"));
    assert!(merged.bindings.contains_key("h"));
}

#[test]
fn unknown_keys_are_rejected_at_every_schema_level() {
    // Phase F5 configuration: deny-unknown-fields protects spelling mistakes and future reloads.
    for document in [
        "mystery = true",
        "[history]\nscrollback-lines = 10\nmystery = 1",
        "[notifications]\nenabled = true\nmystery = true",
        "[[hooks]]\nname = 'watch'\nmystery = true\ncommand = { argv = ['x'] }",
        "[bindings.x]\nbuiltin = 'help'\nmystery = true",
    ] {
        assert!(
            matches!(Config::from_toml(document), Err(ConfigError::Toml(_))),
            "accepted: {document}"
        );
    }
}

#[test]
fn invalid_reload_does_not_mutate_the_live_configuration() {
    // Phase F5 configuration: validate the complete candidate before live replacement.
    let live = Config::default();
    let before = live.clone();
    assert!(live.merge_toml("default-command = { argv = [] }").is_err());
    assert_eq!(live, before);
    assert!(
        live.merge_toml("remote-allow-ids = ['same', 'same']")
            .is_err()
    );
    assert_eq!(live, before);
}

#[test]
fn bindings_hooks_and_commands_enforce_execution_bounds() {
    // Phase F5 Bindings and hooks: commands are non-empty and hooks have stable safe names.
    assert!(Config::from_toml("[bindings.x]\nexternal = { argv = [] }").is_err());
    assert!(
        Config::from_toml("hooks = [{ name = '../bad', command = { argv = ['x'] } }]").is_err()
    );
    assert!(Config::from_toml("hooks = [{ name = 'same', command = { argv = ['x'] } }, { name = 'same', command = { argv = ['y'] } }]").is_err());
    assert!(Config::from_toml("prefix = 'x'\n[bindings.x]\nbuiltin = 'help'").is_err());
}

#[test]
fn initial_load_rejects_keys_the_runtime_router_cannot_encode() {
    let root = std::env::temp_dir().join(format!(
        "fux-config-invalid-binding-keys-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir(&root).expect("root");
    let path = root.join("config.toml");
    for source in [
        "prefix = 'ab'",
        "prefix = 'λ'",
        "[bindings.ab]\nbuiltin = 'help'",
        "[bindings.'λ']\nbuiltin = 'help'",
        "[bindings.'C-ab']\nbuiltin = 'help'",
    ] {
        fs::write(&path, source).expect("write invalid config");
        assert!(
            Config::load_from_path(&path).is_err(),
            "unexpectedly accepted {source:?}"
        );
    }
    fs::write(&path, "prefix = 'C-x'\n[bindings.q]\nbuiltin = 'help'").expect("write valid config");
    Config::load_from_path(&path).expect("one-byte and C-x keys");
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn configured_tab_and_popup_caps_never_exceed_state_hard_limits() {
    assert!(Config::from_toml("[resources]\nmax-tabs = 65").is_err());
    assert!(Config::from_toml("[resources]\nmax-popups = 33").is_err());
    assert!(Config::from_toml("[resources]\nmax-tabs = 64\nmax-popups = 32").is_ok());
    assert!(Config::from_toml("[resources]\nmax-panes = 257").is_err());
    assert!(Config::from_toml("[resources]\nmax-total-cells = 262145").is_err());
    assert!(Config::from_toml("[resources]\nmax-panes = 256\nmax-total-cells = 262144").is_ok());
}

#[test]
fn complete_example_round_trips_without_losing_policy_or_limits() {
    // Phase F5 configuration: the frozen schema has a stable human-readable TOML round trip.
    let source = r#"
prefix = "C-b"
default-command = { argv = ["/bin/zsh", "-l"] }
zor-path = "/opt/bin/zor"
clipboard = "read-only"
remote-allow-ids = ["endpoint-one"]
hooks = [{ name = "status", command = { argv = ["status-helper", "--watch"] } }]

[bindings."C-p"]
builtin = "workspace-picker"

[bindings."C-e"]
external = { argv = ["external-helper"] }

[notifications]
enabled = true
notify-blocked = true
notify-idle = false
remote-clients = false

[history]
scrollback-lines = 20000
capture-bytes = 131072

[resources]
max-units = 33554432
max-panes = 64
max-tabs = 16
max-popups = 8
max-status-segments = 16
max-total-cells = 200000
"#;
    let parsed = Config::from_toml(source).expect("complete example");
    let encoded = parsed.to_toml_pretty().expect("serialize complete config");
    let reparsed = Config::from_toml(&encoded).expect("reparse complete config");
    assert_eq!(reparsed, parsed);
}

#[test]
fn missing_file_uses_defaults_and_existing_file_is_loaded() {
    // Phase F5 configuration: a config file is optional and a present file is applied.
    let directory = std::env::temp_dir().join(format!("fux-config-test-{}", std::process::id()));
    let path = directory.join("config.toml");
    let _ = fs::remove_dir_all(&directory);
    assert_eq!(
        Config::load_from_path(&path).expect("missing config"),
        Config::default()
    );
    fs::create_dir_all(&directory).expect("create test directory");
    fs::write(&path, "prefix = 'C-z'").expect("write config");
    assert_eq!(
        Config::load_from_path(&path).expect("load config").prefix,
        "C-z"
    );
    fs::remove_dir_all(&directory).expect("remove test directory");
}
