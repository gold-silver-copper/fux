//! Parsed public-surface guard for zor's typed fux client.
use std::collections::BTreeSet;
use syn::visit::{self, Visit};

fn escapes(vis: &syn::Visibility) -> bool {
    match vis {
        syn::Visibility::Inherited => false,
        syn::Visibility::Restricted(v) => !v.path.is_ident("super") && !v.path.is_ident("self"),
        syn::Visibility::Public(_) => true,
    }
}

#[derive(Default)]
struct Surface {
    aliases: BTreeSet<String>,
    failures: Vec<String>,
}
impl<'ast> Visit<'ast> for Surface {
    fn visit_path(&mut self, node: &'ast syn::Path) {
        let parts = node
            .segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect::<Vec<_>>();
        self.check_policy_path(&parts);
        visit::visit_path(self, node);
    }
    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        for parts in use_paths(&node.tree, Vec::new()) {
            self.check_policy_path(&parts);
        }
        visit::visit_item_use(self, node);
    }
    fn visit_use_rename(&mut self, node: &'ast syn::UseRename) {
        if node.ident == "Value" {
            self.aliases.insert(node.rename.to_string());
        }
        visit::visit_use_rename(self, node);
    }
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if escapes(&node.vis) {
            let mut types = RawType {
                aliases: &self.aliases,
                found: false,
            };
            types.visit_signature(&node.sig);
            if types.found {
                self.failures
                    .push(format!("{} exposes arbitrary JSON", node.sig.ident));
            }
        }
        visit::visit_item_fn(self, node);
    }
    fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
        // A typed public wrapper with a private raw field still leaks an untyped contract.
        if escapes(&node.vis) {
            for field in &node.fields {
                let mut types = RawType {
                    aliases: &self.aliases,
                    found: false,
                };
                types.visit_type(&field.ty);
                if types.found {
                    self.failures
                        .push(format!("{} wraps arbitrary JSON", node.ident));
                }
            }
        }
        visit::visit_item_struct(self, node);
    }
    fn visit_item_enum(&mut self, node: &'ast syn::ItemEnum) {
        if escapes(&node.vis) {
            for variant in &node.variants {
                for field in &variant.fields {
                    let mut types = RawType {
                        aliases: &self.aliases,
                        found: false,
                    };
                    types.visit_type(&field.ty);
                    if types.found {
                        self.failures
                            .push(format!("{} exposes arbitrary JSON", node.ident));
                    }
                }
            }
        }
        visit::visit_item_enum(self, node);
    }
    fn visit_item_type(&mut self, node: &'ast syn::ItemType) {
        if escapes(&node.vis) && self.aliases.contains(&node.ident.to_string()) {
            self.failures
                .push(format!("{} exposes arbitrary JSON", node.ident));
        }
        visit::visit_item_type(self, node);
    }
    fn visit_field(&mut self, node: &'ast syn::Field) {
        if escapes(&node.vis) {
            let mut types = RawType {
                aliases: &self.aliases,
                found: false,
            };
            types.visit_type(&node.ty);
            if types.found {
                self.failures
                    .push("public field exposes arbitrary JSON".into());
            }
        }
        visit::visit_field(self, node);
    }
}
impl Surface {
    fn check_policy_path(&mut self, parts: &[String]) {
        if parts
            .first()
            .is_some_and(|part| part == "crate" || part == "super")
            && parts.iter().any(|part| {
                [
                    "tasks",
                    "service",
                    "run",
                    "watch",
                    "observe",
                    "dashboard",
                    "pty",
                    "emit",
                    "state",
                ]
                .contains(&part.as_str())
            })
        {
            self.failures.push(format!(
                "client imports application policy: {}",
                parts.join("::")
            ));
        }
    }
}
fn use_paths(tree: &syn::UseTree, mut prefix: Vec<String>) -> Vec<Vec<String>> {
    match tree {
        syn::UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            use_paths(&path.tree, prefix)
        }
        syn::UseTree::Name(name) => {
            prefix.push(name.ident.to_string());
            vec![prefix]
        }
        syn::UseTree::Rename(rename) => {
            prefix.push(rename.ident.to_string());
            vec![prefix]
        }
        syn::UseTree::Group(group) => group
            .items
            .iter()
            .flat_map(|tree| use_paths(tree, prefix.clone()))
            .collect(),
        syn::UseTree::Glob(_) => vec![prefix],
    }
}

struct RawType<'a> {
    aliases: &'a BTreeSet<String>,
    found: bool,
}
impl<'ast> Visit<'ast> for RawType<'_> {
    fn visit_type_path(&mut self, node: &'ast syn::TypePath) {
        if node.path.segments.last().is_some_and(|segment| {
            segment.ident == "Value" || self.aliases.contains(&segment.ident.to_string())
        }) {
            self.found = true;
        }
        visit::visit_type_path(self, node);
    }
}
fn inspect(source: &str) -> Vec<String> {
    let file = syn::parse_file(source).expect("parse typed client source");
    let mut surface = Surface::default();
    // Collect renamed imports before checking signatures, independent of declaration order.
    for item in &file.items {
        if let syn::Item::Use(item) = item {
            surface.visit_item_use(item);
        }
        if let syn::Item::Type(alias) = item {
            let mut raw = RawType {
                aliases: &surface.aliases,
                found: false,
            };
            raw.visit_type(&alias.ty);
            if raw.found {
                surface.aliases.insert(alias.ident.to_string());
            }
        }
    }
    surface.visit_file(&file);
    surface.failures
}

#[test]
fn fux_client_cannot_export_untyped_requests_or_reply_payloads() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../zor/src");
    let mut files = vec![root.join("fux.rs")];
    files.extend(
        std::fs::read_dir(root.join("fux"))
            .expect("client modules")
            .map(|entry| entry.expect("client entry").path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "rs")),
    );
    for path in files {
        let source = std::fs::read_to_string(&path).expect("read client");
        let source = super::strip_test_modules(&source);
        let failures = inspect(source);
        assert!(failures.is_empty(), "{}: {failures:?}", path.display());
    }
}

#[test]
fn surface_guard_rejects_nested_and_renamed_json_but_allows_private_wire_helpers() {
    for source in [
        "pub(crate) fn request(v: serde_json::Value) {}",
        "pub fn reply() -> Result<Vec<Value>, Error> {}",
        "pub enum Reply { Complete(Value) }",
        "pub struct Reply { hidden: serde_json::Value }",
        "pub type Reply = serde_json::Value;",
        "use serde_json::Value as Wire; pub struct Reply { pub data: Option<Wire> }",
        "type Wire = serde_json::Value; pub fn reply() -> Wire {}",
    ] {
        assert!(!inspect(source).is_empty(), "missed {source}");
    }
    assert!(inspect("fn decode(v: Value) {} pub(super) fn internal(v: Value) {} pub fn list() -> Result<Listing> {}").is_empty());
}

#[test]
fn client_layer_cannot_import_task_policy_through_paths_or_grouped_renames() {
    for source in [
        "use crate::{tasks::Task as Neutral, fux::input::Receipt};",
        "fn f() { crate::service::stop(); }",
        "use super::super::tasks::*;",
    ] {
        assert!(!inspect(source).is_empty(), "missed {source}");
    }
    assert!(
        inspect("use crate::rules::view::Captured; fn f() { super::manager::names(); }").is_empty()
    );
}
