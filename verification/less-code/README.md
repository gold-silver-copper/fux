# Less-code PR evidence

Base: `64adda9f76c55b1dc9b1d04201ba3b5f9c857827` (PR #26 merge).
Preflight: read review-2026-09-20 and PRs #24–26. Existing untracked `docs/`
files were preserved and are not part of this branch's changes.

## Reproducible production count

The measurement tool is separate from the application dependency graph:

```sh
python3 -m venv /tmp/fux-loc-env
/tmp/fux-loc-env/bin/pip install tree-sitter==0.26.0 tree-sitter-rust==0.24.2
/tmp/fux-loc-env/bin/python verification/less-code/count.py main
/tmp/fux-loc-env/bin/python verification/less-code/count.py
```

The Rust syntax tree excludes complete `#[cfg(test)]` items, comments (including
nested block comments), test files/subtrees, and `testing.rs`. Attributes and
code with trailing comments count. Raw-string contents are not mistaken for
comments/braces. Parsing errors fail measurement. Built-in assertions validate
those cases on every invocation. Files under nested production directories are
included. A revision argument reads sources directly from Git, not the worktree.

| State | Production lines | Change from base |
| --- | ---: | ---: |
| Base | 6882 | 0 |
| Section 1 | 6846 | -36 |

## Section 1

Required nodes replace identical spawn arguments. A workspace created by
`MoveTo::NewWorkspace` deliberately retains its explicit **tab** node: its basis
was Auto, unlike the root node's zero basis. Normalization still copies custom
layout into the first tab. Column splits override the row default. No node
mutation in the existing tab-as-split path was removed.

Reference: Bevy 0.19.1 `examples/showcase/breakout.rs` (`Wall` requirements) and
`crates/bevy_ecs/src/component/required.rs` (required constructors and precedence).

Verification: new required-node/explicit-override/removal unit test; existing
native-layout scene roundtrip, legacy normalization, split and multi-viewer
integration tests. All four gates pass (36 unit tests, 35 integration tests).
Gate log: `section1.log`.
