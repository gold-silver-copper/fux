//! Reviewed declaration inventory plus semantic tripwires; runtime fux never depends on these parsers.
#![allow(clippy::expect_used, clippy::panic)]
use quote::ToTokens;
use std::{collections::BTreeMap, fs, path::Path};
use syn::visit::{self, Visit};

#[derive(Default)]
struct Boundary {
    declarations: BTreeMap<String, String>,
    macro_counts: BTreeMap<String, usize>,
    violations: Vec<String>,
}
fn test_only(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr
                .parse_args::<syn::Path>()
                .is_ok_and(|path| path.is_ident("test"))
    })
}
fn docs_removed(attrs: &mut Vec<syn::Attribute>) {
    attrs.retain(|attr| !attr.path().is_ident("doc"));
}
fn clean_fields(fields: &mut syn::Fields) {
    for field in fields {
        docs_removed(&mut field.attrs);
    }
}
fn agent_name(name: &str) -> bool {
    let name = name.strip_prefix("r#").unwrap_or(name);
    let mut words = String::new();
    for ch in name.chars() {
        if ch.is_ascii_uppercase() {
            words.push('_');
        }
        words.push(ch.to_ascii_lowercase());
    }
    words.split('_').any(|word| {
        matches!(
            word,
            "agent"
                | "agents"
                | "worktree"
                | "worktrees"
                | "orchestration"
                | "zor"
                | "koh"
                | "codex"
                | "claude"
                | "opencode"
        )
    })
}
impl Boundary {
    fn tokens(&mut self, tokens: proc_macro2::TokenStream) {
        for token in tokens {
            match token {
                proc_macro2::TokenTree::Group(group) => self.tokens(group.stream()),
                proc_macro2::TokenTree::Ident(ident) => self.visit_ident(&ident),
                proc_macro2::TokenTree::Literal(literal) => {
                    if let Ok(literal) = syn::parse_str::<syn::Lit>(&literal.to_string()) {
                        self.visit_lit(&literal);
                    }
                }
                proc_macro2::TokenTree::Punct(_) => {}
            }
        }
    }
    fn record(&mut self, name: String, tokens: String) {
        assert!(
            self.declarations.insert(name.clone(), tokens).is_none(),
            "duplicate boundary declaration {name}"
        );
    }
    fn literal(&mut self, value: &str) {
        if matches!(
            value,
            "git" | "/usr/bin/git" | "zor" | "koh" | "codex" | "claude" | "opencode" | "7877"
        ) || value.starts_with("agent.")
            || value.starts_with("ZOR_")
            || value.contains("]7877;")
        {
            self.violations
                .push(format!("agent/transport/git literal {value:?}"));
        }
    }
}
impl<'ast> Visit<'ast> for Boundary {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if !test_only(&node.attrs) {
            visit::visit_item_mod(self, node);
        }
    }
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if !test_only(&node.attrs) {
            visit::visit_item_fn(self, node);
        }
    }
    fn visit_attribute(&mut self, node: &'ast syn::Attribute) {
        if !node.path().is_ident("doc") {
            self.tokens(node.meta.to_token_stream());
            visit::visit_attribute(self, node);
        }
    }
    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        let path = node.path.to_token_stream().to_string();
        let ordinal = self.macro_counts.entry(path.clone()).or_default();
        *ordinal += 1;
        let key = format!("macro {path} #{ordinal}");
        self.record(key, node.to_token_stream().to_string());
        self.tokens(node.tokens.clone());
        visit::visit_macro(self, node);
    }
    fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
        if test_only(&node.attrs) {
            return;
        }
        let mut node = node.clone();
        docs_removed(&mut node.attrs);
        clean_fields(&mut node.fields);
        self.record(
            format!("struct {}", node.ident),
            node.to_token_stream().to_string(),
        );
        visit::visit_item_struct(self, &node);
    }
    fn visit_item_enum(&mut self, node: &'ast syn::ItemEnum) {
        if test_only(&node.attrs) {
            return;
        }
        let mut node = node.clone();
        docs_removed(&mut node.attrs);
        for variant in &mut node.variants {
            docs_removed(&mut variant.attrs);
            clean_fields(&mut variant.fields);
        }
        self.record(
            format!("enum {}", node.ident),
            node.to_token_stream().to_string(),
        );
        visit::visit_item_enum(self, &node);
    }
    fn visit_item_type(&mut self, node: &'ast syn::ItemType) {
        if test_only(&node.attrs) {
            return;
        }
        let mut node = node.clone();
        docs_removed(&mut node.attrs);
        self.record(
            format!("type {}", node.ident),
            node.to_token_stream().to_string(),
        );
        visit::visit_item_type(self, &node);
    }
    fn visit_ident(&mut self, node: &'ast syn::Ident) {
        if agent_name(&node.to_string()) {
            self.violations
                .push(format!("agent/transport identifier {node}"));
        }
    }
    fn visit_lit_str(&mut self, node: &'ast syn::LitStr) {
        self.literal(&node.value());
    }
    fn visit_lit_byte_str(&mut self, node: &'ast syn::LitByteStr) {
        self.literal(&String::from_utf8_lossy(&node.value()));
    }
    fn visit_lit_int(&mut self, node: &'ast syn::LitInt) {
        if node.base10_parse::<u64>().is_ok_and(|value| value == 7877) {
            self.violations.push("agent OSC selector 7877".into());
        }
    }
}
fn scan(source: &str) -> Boundary {
    let file = syn::parse_file(source).expect("parse Rust source for ownership review");
    let mut boundary = Boundary::default();
    boundary.visit_file(&file);
    boundary
}
fn sources(directory: &Path, output: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(directory).expect("source directory") {
        let path = entry.expect("source entry").path();
        if path.is_dir() {
            sources(&path, output);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            output.push(path);
        }
    }
}
#[test]
fn multiplexer_declarations_match_the_reviewed_boundary() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    sources(&root.join("src"), &mut files);
    let mut inventory = BTreeMap::new();
    for path in files {
        let boundary = scan(&fs::read_to_string(&path).expect("source bytes"));
        assert!(
            boundary.violations.is_empty(),
            "{}: {:?}",
            path.display(),
            boundary.violations
        );
        for (name, declaration) in boundary.declarations {
            inventory.insert(
                format!(
                    "{}::{name}",
                    path.strip_prefix(root)
                        .expect("source under root")
                        .display()
                ),
                declaration,
            );
        }
    }
    let path = root.join("tests/fixtures/multiplexer-boundary.json");
    if std::env::var_os("FUX_UPDATE_BOUNDARY").as_deref() == Some(std::ffi::OsStr::new("1")) {
        fs::create_dir_all(path.parent().expect("fixture parent")).expect("fixture directory");
        fs::write(
            &path,
            serde_json::to_string_pretty(&inventory).expect("inventory JSON") + "\n",
        )
        .expect("write reviewed inventory");
    }
    let expected: BTreeMap<String, String> =
        serde_json::from_str(&fs::read_to_string(path).expect("reviewed declaration inventory"))
            .expect("inventory JSON");
    let changed: Vec<_> = inventory
        .keys()
        .chain(expected.keys())
        .filter(|key| inventory.get(*key) != expected.get(*key))
        .collect();
    assert!(
        changed.is_empty(),
        "review the semantic ownership of changed declarations before updating the inventory: {changed:?}"
    );
}
#[test]
fn boundary_detects_semantics_and_generic_disguised_model_additions() {
    assert!(
        !scan("struct AgentState { ready: bool }")
            .violations
            .is_empty()
    );
    assert!(!scan("use zor as controller;").violations.is_empty());
    assert!(!scan("use r#zor as controller;").violations.is_empty());
    assert!(
        !scan("fn run() { let args = vec![\"zor\"]; }")
            .violations
            .is_empty()
    );
    assert!(
        !scan("fn probe() { let selector = 0x1ec5; }")
            .violations
            .is_empty()
    );
    assert!(
        !scan("fn run() { std::process::Command::new(\"git\"); }")
            .violations
            .is_empty()
    );
    let before = scan("struct Pane { pid: u32 }");
    let after =
        scan("struct Pane { pid: u32, metadata: std::collections::BTreeMap<String,String> }");
    assert!(after.violations.is_empty());
    assert_ne!(
        before.declarations, after.declarations,
        "new generic metadata still requires semantic review"
    );
    let generated = scan(
        "macro_rules! generated { () => { struct Metadata { values: Vec<String> } }; } generated!();",
    );
    assert!(generated.violations.is_empty());
    assert!(
        !generated.declarations.is_empty(),
        "generic macro expansion must require ownership review"
    );
    assert!(scan("// agent state belongs outside fux\nstruct Magenta; #[cfg(test)] mod tests { struct AgentState; }").violations.is_empty());
}
#[test]
fn agent_osc_reports_have_no_semantic_effect_on_the_multiplexer() {
    let mut terminal = fux::terminal::ServerTerminal::new(8, 40, 100);
    terminal.process(b"shell output\x1b]2;ordinary title\x07\x1b]9;4;1;25\x07");
    let capture = terminal.capture(0, false, 4096);
    let progress = terminal.progress();
    for report in [
        b"\x1b]7877;v=1;agent=codex;state=working\x07".as_slice(),
        b"\x1b]7877;v=1;agent=claude;state=blocked\x1b\\".as_slice(),
    ] {
        for byte in report {
            terminal.process(&[*byte]);
        }
        assert_eq!(terminal.title(), "ordinary title");
        assert_eq!(terminal.progress(), progress);
        assert_eq!(terminal.capture(0, false, 4096), capture);
        assert!(terminal.take_host_replies().is_empty());
    }
}
