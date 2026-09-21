use crate::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io::Write,
    path::Path,
    time::Instant,
};

// Versioned scenario recipes, not a general scenario language. All varying
// choices are stored here; replay never calls the generator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub version: u32,
    pub seed: u64,
    pub actions: Vec<Action>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scenario", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Startup {
        config: Config,
    },
    Resize {
        sizes: Vec<[u16; 4]>,
        token: String,
    },
    Shutdown {
        mode: Shutdown,
    },
    Paste {
        text: String,
        repeats: usize,
        bracketed: bool,
        chunk_bytes: usize,
    },
    /// A graceful termination signal delivered to an attached frontend.
    Signal {
        signal: FrontendSignal,
    },
    /// Raw outer-terminal key encodings that must reach the pane byte-exact.
    Keys {
        sequences: Vec<String>,
    },
    /// Copy-mode selections whose OSC 52 payload must decode to the selected text.
    Copy {
        /// Start with the clipboard disabled and enable it by creating the
        /// configuration file while the server runs.
        reload: bool,
    },
    /// History scrolling by command, wheel and copy-mode paging, plus copying
    /// from history and the selection-invalidation rule.
    History,
    /// Viewer-local zoom with two viewers sharing two panes.
    Zoom,
    /// Pane moves, tab/workspace closes, shared references and confirmations.
    Layout,
    /// Stock-spawned Launch recipes, reflected dimension edits, termination
    /// paths, failed launches, natural exit and history_lines.
    Process,
    /// Directional focus ranking, focus cycling and swaps.
    Nav,
    /// Layout save and load, including failures that must not replace it.
    Scene,
    /// Hot reload of prefix and bindings.
    Config,
    /// Prompts, confirmations, choosers and unavailable actions.
    Overlay,
    /// Documented hard limits: attach clamping, copy cell cap, clipboard
    /// policy and the paste bound.
    Limits,
    /// Painting rules at narrow sizes: wide glyphs, viewport bounds, tab bar.
    Chrome,
    /// Selection invalidation when the view changes underneath it.
    Selection,
    /// Commands racing an in-flight scene load, a prompt whose target another
    /// viewer closes, and detaching during copy mode.
    Race,
    /// Two viewers' independent tab selection and per-tab focus memory.
    Memory,
    /// Tab, workspace and pane reorder invariants.
    Reorder,
    /// Layout loads with explicit, duplicate, misdirected and missing
    /// mappings, and the configured `layout:` reload path.
    SceneMap,
    /// Mouse events beyond the legacy encoding range, Shift-right-click while
    /// the application owns the mouse, wheel on chrome, and API coordinates
    /// outside the viewer.
    MouseEdge,
    /// The 16-entry clipboard delivery queue and a reload that disables it.
    ClipQueue,
    /// Ctrl+arrow pane resizing: conservation, minimums and no-op axes.
    ResizeCmd,
    /// Malformed API requests rejected at deserialization, and zero or
    /// oversized viewports.
    ApiMisuse,
    /// Save, load, save: structural equality and custom Node fidelity.
    SceneFidelity,
    /// A tabless (PR #20) scene wrapped into one tab, stable on round trip.
    Tabless,
    /// Configuration and layout file churn under the asset watcher.
    Churn,
    /// Scene process references dying between validation and application.
    SceneRefs,
    /// One server through repeated lifecycle cycles with invariants after each.
    Soak {
        cycles: usize,
    },
    /// Viewer relationship repair under raw API despawns.
    Repair,
    /// Alternate screen, application cursor keys, self-resizing children and
    /// 8-bit C1 bytes through a real child.
    TerminalEdge,
    /// A frontend whose outer PTY is not read under hot output, then resumes.
    Stream,
    /// A seeded random walk over the command set with invariants after
    /// every step. Steps are stored so replay is exact.
    Walk {
        seed: u64,
        steps: Vec<Step>,
    },
    /// Hundreds of panes, a thousand tabs, fifty workspaces, huge names and
    /// pastes, and a scene round trip of the large layout.
    Scale,
    /// A seeded adversarial byte stream through a real child under resize
    /// and scroll.
    Adversarial {
        seed: u64,
    },
    /// Two frontends each walking their own seeded steps, interleaved.
    Concurrent {
        seed: u64,
        steps: Vec<Step>,
    },
    /// Ordinary steps interleaved with raw API mutations the README permits
    /// but does not own, judged by the narrow oracle: no panic, every viewer
    /// keeps painting, the driver's relationships are repaired, and the next
    /// ordinary step succeeds or reports a documented notice.
    Raw {
        seed: u64,
        steps: Vec<Step>,
    },
    /// A seeded mutator over a saved scene file: each case is loaded into a
    /// live workspace and must either apply cleanly or be refused untouched.
    SceneFuzz {
        seed: u64,
        cases: Vec<SceneCase>,
    },
    /// Outer-terminal mouse events against a pane that requested a protocol.
    Mouse {
        /// The DECSET the child requests: 1000, 1002 or 1003.
        mode: u16,
        /// Whether the child also requests SGR (1006) encoding.
        sgr: bool,
        /// Two side-by-side panes instead of one full-width pane.
        split: bool,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontendSignal {
    Interrupt,
    Terminate,
    Hangup,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Config {
    Valid,
    Missing,
    Malformed,
    /// No shell override; only `clipboard: write-only`.
    Clipboard,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shutdown {
    Initializing,
    Lifecycle,
    Output,
}

/// One generated command. Indices are resolved modulo the live counts when
/// the step runs, so the same trace makes the same choices on replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Split {
        horizontal: bool,
    },
    ClosePane(u8),
    CloseTab(u8),
    CloseWorkspace(u8),
    Terminate,
    Zoom,
    Rename(u8),
    Resize {
        horizontal: bool,
        grow: bool,
    },
    ReorderPane(u8),
    Reorder {
        tab: bool,
        next: bool,
    },
    SwapDirection(u8),
    MoveDirection(u8),
    MoveTo {
        kind: u8,
        index: u8,
    },
    Select {
        tab: bool,
        index: u8,
    },
    Next {
        tab: bool,
    },
    Previous {
        tab: bool,
    },
    Focus(u8),
    FocusNext,
    FocusPrevious,
    FocusLast,
    FocusDirection(u8),
    Scroll(u8),
    CopyMode,
    LeaveCopyMode,
    Save,
    Load,
    Help,
    Menu,
    Choose,
    AttachViewer,
    DetachViewer,
    Key(u8),
    TabNew,
    WorkspaceNew,
    /// The same command expressed as keystrokes on the driver's real
    /// frontend: the prefix, the bound key, and any prompt answered by typing.
    Typed(Keyed),
    /// A lone Escape, outside an overlay or inside the command column.
    LoneEscape {
        inside: bool,
    },
    /// The prefix pressed twice: one literal byte to the pane.
    DoublePrefix,
    /// An unbound key under the prefix leaves the column open.
    UnknownPrefixKey,
    /// A bracketed paste of shortcut letters into the open command column.
    PasteInColumn,
    /// An SGR mouse event on the frontend, placed from the current paint.
    Mouse {
        kind: u8,
        at: u8,
    },
    /// The frontend's outer PTY resized to a size from a table that includes
    /// 2x2; `ClampViewer` covers the 4096 clamp through an API viewer.
    ViewerResize(u8),
    ClampViewer,
    /// A child that exits on its own with a code, by typing `exit N` into
    /// the shell when it is focused, or by launching a program that exits.
    ChildExit(u8),
    /// A child that changes its own terminal state: alternate screen, mouse
    /// reporting, application cursor keys, bracketed paste, or `stty`.
    ChildMode(u8),
    /// A configuration rewrite: prefix, clipboard, a watched `layout:` that
    /// names the walk's own save file, a malformed file, then a valid one.
    Config(u8),
    /// A second real frontend attached mid-walk, and one killed with SIGHUP.
    AttachFrontend,
    HangupFrontend,
    /// A raw API mutation; only `Action::Raw` walks generate these.
    Raw {
        kind: u8,
        index: u8,
    },
}

/// A command with a keyboard path through the real frontend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Keyed {
    Split {
        horizontal: bool,
    },
    /// `x`, then `y` or `n`.
    Close {
        confirm: bool,
    },
    FocusNext,
    FocusPrevious,
    FocusLast,
    FocusDirection(u8),
    Zoom,
    /// `r`, a typed name, then Enter or Escape.
    Rename {
        accept: bool,
        wide: bool,
    },
    Resize {
        horizontal: bool,
        grow: bool,
    },
    MoveDirection(u8),
    TabNew,
    TabNext,
    TabPrevious,
    WorkspaceNew,
    WorkspaceNext,
    WorkspacePrevious,
    CopyMode,
    /// Keys inside copy mode: movement, anchor, copy, leave.
    CopyKey(u8),
    Copy,
    /// `T` or `W`, `j` presses, then Enter or `q`.
    Choose {
        tab: bool,
        entry: u8,
        accept: bool,
    },
    /// `p`, `s` or `S`, `j` presses, Enter or `q`, and any follow-up prompt.
    Menu {
        which: u8,
        entry: u8,
        accept: bool,
    },
}

/// One mutation of a saved scene file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SceneCase {
    DropBlock(u8),
    DuplicateBlock(u8),
    SwapIds(u8, u8),
    DanglingChildOf(u8),
    SplitOneChild(u8),
    SplitAncestorChild(u8),
    FlexZero(u8),
    FlexNegative(u8),
    FlexNan(u8),
    GapThousand(u8),
    HugeName(u8),
    StripTab,
    Truncate(u8),
}

/// Which step kinds a generator may draw.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// Everything but raw mutations.
    Walk,
    /// Two walkers share one server: no configuration rewrites, extra
    /// frontends or clamp viewers, which would race the other walker.
    Concurrent,
    /// BRP steps interleaved with raw mutations.
    Raw,
}

pub fn generate_cases(seed: u64, count: usize) -> Vec<SceneCase> {
    let mut state = seed ^ 0x5ce4e;
    (0..count)
        .map(|_| {
            let r = splitmix(&mut state);
            let a = ((r >> 8) & 0xff) as u8;
            let b = ((r >> 16) & 0xff) as u8;
            match r % 13 {
                0 => SceneCase::DropBlock(a),
                1 => SceneCase::DuplicateBlock(a),
                2 => SceneCase::SwapIds(a, b),
                3 => SceneCase::DanglingChildOf(a),
                4 => SceneCase::SplitOneChild(a),
                5 => SceneCase::SplitAncestorChild(a),
                6 => SceneCase::FlexZero(a),
                7 => SceneCase::FlexNegative(a),
                8 => SceneCase::FlexNan(a),
                9 => SceneCase::GapThousand(a),
                10 => SceneCase::HugeName(a),
                11 => SceneCase::StripTab,
                _ => SceneCase::Truncate(a),
            }
        })
        .collect()
}

fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e3779b97f4a7c15);
    let mut x = *state;
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

/// Weighted so structure changes are common and closes keep the tree bounded.
pub fn generate_steps(seed: u64, count: usize) -> Vec<Step> {
    generate_steps_for(Profile::Walk, seed, count)
}

/// The keyboard form of a command, for the share of steps that go through
/// the frontend instead of BRP.
fn keyed(r: u64, a: u8, b: u8) -> Keyed {
    match (r % 24) as u8 {
        0 | 1 => Keyed::Split {
            horizontal: a.is_multiple_of(2),
        },
        2 => Keyed::Close {
            confirm: b.is_multiple_of(2),
        },
        3 => Keyed::FocusNext,
        4 => Keyed::FocusPrevious,
        5 => Keyed::FocusLast,
        6 => Keyed::FocusDirection(a),
        7 => Keyed::Zoom,
        8 => Keyed::Rename {
            accept: a.is_multiple_of(2),
            wide: b.is_multiple_of(3),
        },
        9 => Keyed::Resize {
            horizontal: a.is_multiple_of(2),
            grow: b.is_multiple_of(2),
        },
        10 => Keyed::MoveDirection(a),
        11 => Keyed::TabNew,
        12 => Keyed::TabNext,
        13 => Keyed::TabPrevious,
        14 => Keyed::WorkspaceNew,
        15 => Keyed::WorkspaceNext,
        16 => Keyed::WorkspacePrevious,
        17 => Keyed::CopyMode,
        18 => Keyed::CopyKey(a),
        19 => Keyed::Copy,
        20 => Keyed::Choose {
            tab: a.is_multiple_of(2),
            entry: b,
            accept: (a >> 1).is_multiple_of(2),
        },
        _ => Keyed::Menu {
            which: a,
            entry: b,
            accept: (a >> 1).is_multiple_of(2),
        },
    }
}

pub fn generate_steps_for(profile: Profile, seed: u64, count: usize) -> Vec<Step> {
    let mut state = seed;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let r = splitmix(&mut state);
        let roll = (r % 100) as u8;
        let a = ((r >> 8) & 0xff) as u8;
        let b = ((r >> 16) & 0xff) as u8;
        // A second draw decides the source and the events outside the
        // command set, so the command distribution below is unchanged.
        let side = splitmix(&mut state);
        let extra = (side % 100) as u8;
        let event = match (profile, extra) {
            (Profile::Raw, 0..=14) => Some(Step::Raw {
                kind: ((side >> 8) & 0xff) as u8,
                index: ((side >> 16) & 0xff) as u8,
            }),
            (Profile::Raw, _) => None,
            (_, 0..=21) => Some(Step::Typed(keyed(side >> 24, a, b))),
            (_, 22) => Some(Step::LoneEscape {
                inside: a.is_multiple_of(2),
            }),
            (_, 23) => Some(Step::DoublePrefix),
            (_, 24) => Some(Step::UnknownPrefixKey),
            (_, 25) => Some(Step::PasteInColumn),
            (_, 26..=29) => Some(Step::Mouse {
                kind: ((side >> 8) & 0xff) as u8,
                at: ((side >> 16) & 0xff) as u8,
            }),
            (_, 30..=32) => Some(Step::ViewerResize(((side >> 8) & 0xff) as u8)),
            (Profile::Walk, 33) => Some(Step::ClampViewer),
            (_, 34) => Some(Step::ChildExit(((side >> 8) & 0xff) as u8)),
            (_, 35) => Some(Step::ChildMode(((side >> 8) & 0xff) as u8)),
            (Profile::Walk, 36) => Some(Step::Config(((side >> 8) & 0xff) as u8)),
            (Profile::Walk, 37) => Some(Step::AttachFrontend),
            (Profile::Walk, 38) => Some(Step::HangupFrontend),
            _ => None,
        };
        if let Some(step) = event {
            out.push(step);
            continue;
        }
        let step = match roll {
            0..=13 => Step::Split {
                horizontal: a.is_multiple_of(2),
            },
            14..=21 => Step::ClosePane(a),
            22..=24 => Step::CloseTab(a),
            25..=26 => Step::CloseWorkspace(a),
            27..=30 => Step::MoveTo { kind: a, index: b },
            31..=33 => Step::SwapDirection(a),
            34..=36 => Step::MoveDirection(a),
            37..=39 => Step::Reorder {
                tab: a.is_multiple_of(2),
                next: b.is_multiple_of(2),
            },
            40..=41 => Step::ReorderPane(a),
            42..=45 => Step::Select {
                tab: a.is_multiple_of(2),
                index: b,
            },
            46..=47 => Step::Next {
                tab: a.is_multiple_of(2),
            },
            48..=49 => Step::Previous {
                tab: a.is_multiple_of(2),
            },
            50..=52 => Step::Focus(a),
            53 => Step::FocusNext,
            54 => Step::FocusPrevious,
            55 => Step::FocusLast,
            56..=58 => Step::FocusDirection(a),
            59..=61 => Step::Zoom,
            62..=65 => Step::Resize {
                horizontal: a.is_multiple_of(2),
                grow: b.is_multiple_of(2),
            },
            66..=68 => Step::TabNew,
            69..=70 => Step::WorkspaceNew,
            71..=72 => Step::Rename(a),
            73..=75 => Step::Save,
            76..=78 => Step::Load,
            79..=80 => Step::Scroll(a),
            81..=82 => Step::CopyMode,
            83..=84 => Step::LeaveCopyMode,
            85 => Step::Help,
            86 => Step::Menu,
            87 => Step::Choose,
            88..=90 => Step::AttachViewer,
            91..=92 => Step::DetachViewer,
            93..=94 => Step::Terminate,
            _ => Step::Key(a),
        };
        out.push(step);
    }
    out
}

impl Plan {
    pub fn generate(scenario: &str, seed: u64, iterations: usize, count: usize) -> Result<Self> {
        ensure(
            matches!(
                scenario,
                "all"
                    | "startup"
                    | "resize"
                    | "shutdown"
                    | "paste"
                    | "signal"
                    | "keys"
                    | "mouse"
                    | "copy"
                    | "history"
                    | "zoom"
                    | "layout"
                    | "process"
                    | "nav"
                    | "scene"
                    | "config"
                    | "overlay"
                    | "limits"
                    | "chrome"
                    | "selection"
                    | "race"
                    | "memory"
                    | "reorder"
                    | "scene_map"
                    | "mouse_edge"
                    | "clipqueue"
                    | "resize_cmd"
                    | "api_misuse"
                    | "scene_fidelity"
                    | "tabless"
                    | "churn"
                    | "scene_refs"
                    | "soak"
                    | "repair"
                    | "terminal_edge"
                    | "stream"
                    | "walk"
                    | "scale"
                    | "adversarial"
                    | "concurrent"
                    | "raw"
                    | "scene_fuzz"
            ),
            "unknown scenario",
        )?;
        ensure(
            (1..=100).contains(&iterations) && (1..=5000).contains(&count),
            "iterations must be 1..100 and actions 1..5000",
        )?;
        let mut state = seed;
        let mut actions = Vec::new();
        for iteration in 0..iterations {
            if matches!(scenario, "all" | "startup") {
                for config in [Config::Missing, Config::Malformed, Config::Valid] {
                    actions.push(Action::Startup { config });
                }
            }
            if matches!(scenario, "all" | "resize") {
                let mut sizes = Vec::new();
                // Each fixed tiny case ends a burst, so it gets a convergence
                // assertion rather than being only an intermediate ioctl.
                for size in [[1, 1, 2, 2], [2, 1, 1, 80], [24, 80, 12, 40]] {
                    sizes.extend([size; 3]);
                }
                for _ in 0..count {
                    let mut next = || {
                        // SplitMix64: zero is a useful seed too.
                        state = state.wrapping_add(0x9e3779b97f4a7c15);
                        let mut x = state;
                        x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
                        x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
                        x ^ (x >> 31)
                    };
                    sizes.push([
                        1 + (next() % 40) as u16,
                        1 + (next() % 120) as u16,
                        1 + (next() % 40) as u16,
                        1 + (next() % 120) as u16,
                    ]);
                }
                sizes.push([24, 80, 18, 60]);
                actions.push(Action::Resize {
                    sizes,
                    token: format!("INPUT-{state:016x}"),
                });
            }
            if matches!(scenario, "all" | "paste") {
                // Payload limit is in UTF-8 bytes, exclusive of terminal framing.
                // The boundary controls distinguish envelope overhead from size
                // rejection, Unicode decoding, fragmentation, or a blocked PTY.
                for (text, repeats, bracketed, chunk_bytes) in [
                    ("x", 0, false, 1),
                    ("line\n界é", 3, true, 1),
                    ("x", 65_524, true, 1024),
                    ("x", 65_525, true, 1024),
                    ("x", 65_536, false, 1024),
                    ("x", 65_536, true, 1024),
                    ("界", 21_845, false, 1024),
                    ("界", 21_845, true, 1024),
                    ("x", 65_537, false, 1024),
                    ("x", 65_537, true, 1024),
                ] {
                    actions.push(Action::Paste {
                        text: text.into(),
                        repeats,
                        bracketed,
                        chunk_bytes,
                    });
                }
            }
            if matches!(scenario, "all" | "keys") {
                let canonical = canonical_keys();
                actions.push(Action::Keys {
                    sequences: canonical.clone(),
                });
                // A seeded order finds escape-timing interactions between
                // neighbours that the canonical order happens to avoid.
                let mut shuffled = canonical;
                for i in (1..shuffled.len()).rev() {
                    state = state.wrapping_add(0x9e3779b97f4a7c15);
                    let mut x = state;
                    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
                    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
                    let j = ((x ^ (x >> 31)) % (i as u64 + 1)) as usize;
                    shuffled.swap(i, j);
                }
                actions.push(Action::Keys {
                    sequences: shuffled,
                });
            }
            if matches!(scenario, "all" | "copy") {
                for reload in [false, true] {
                    actions.push(Action::Copy { reload });
                }
            }
            if matches!(scenario, "all" | "history") {
                actions.push(Action::History);
            }
            if matches!(scenario, "all" | "zoom") {
                actions.push(Action::Zoom);
            }
            if matches!(scenario, "all" | "layout") {
                actions.push(Action::Layout);
            }
            if matches!(scenario, "all" | "process") {
                actions.push(Action::Process);
            }
            if matches!(scenario, "all" | "nav") {
                actions.push(Action::Nav);
            }
            if matches!(scenario, "all" | "scene") {
                actions.push(Action::Scene);
            }
            if matches!(scenario, "all" | "config") {
                actions.push(Action::Config);
            }
            if matches!(scenario, "all" | "overlay") {
                actions.push(Action::Overlay);
            }
            if matches!(scenario, "all" | "limits") {
                actions.push(Action::Limits);
            }
            if matches!(scenario, "all" | "chrome") {
                actions.push(Action::Chrome);
            }
            if matches!(scenario, "all" | "selection") {
                actions.push(Action::Selection);
            }
            if matches!(scenario, "all" | "race") {
                actions.push(Action::Race);
            }
            if matches!(scenario, "all" | "memory") {
                actions.push(Action::Memory);
            }
            if matches!(scenario, "all" | "reorder") {
                actions.push(Action::Reorder);
            }
            if matches!(scenario, "all" | "scene_map") {
                actions.push(Action::SceneMap);
            }
            if matches!(scenario, "all" | "mouse_edge") {
                actions.push(Action::MouseEdge);
            }
            if matches!(scenario, "all" | "clipqueue") {
                actions.push(Action::ClipQueue);
            }
            if matches!(scenario, "all" | "resize_cmd") {
                actions.push(Action::ResizeCmd);
            }
            if matches!(scenario, "all" | "api_misuse") {
                actions.push(Action::ApiMisuse);
            }
            if matches!(scenario, "all" | "scene_fidelity") {
                actions.push(Action::SceneFidelity);
            }
            if matches!(scenario, "all" | "tabless") {
                actions.push(Action::Tabless);
            }
            if matches!(scenario, "all" | "churn") {
                actions.push(Action::Churn);
            }
            if matches!(scenario, "all" | "scene_refs") {
                actions.push(Action::SceneRefs);
            }
            if matches!(scenario, "all" | "soak") {
                actions.push(Action::Soak { cycles: 8 });
            }
            if matches!(scenario, "all" | "repair") {
                actions.push(Action::Repair);
            }
            if matches!(scenario, "all" | "terminal_edge") {
                actions.push(Action::TerminalEdge);
            }
            if matches!(scenario, "all" | "stream") {
                actions.push(Action::Stream);
            }
            if matches!(scenario, "all" | "walk") {
                let steps = if count <= 6 { 120 } else { count };
                actions.push(Action::Walk {
                    seed: seed.wrapping_add(iteration as u64),
                    steps: generate_steps(seed.wrapping_add(iteration as u64), steps),
                });
            }
            if matches!(scenario, "all" | "scale") {
                actions.push(Action::Scale);
            }
            if matches!(scenario, "all" | "adversarial") {
                actions.push(Action::Adversarial {
                    seed: seed.wrapping_add(iteration as u64),
                });
            }
            if matches!(scenario, "all" | "concurrent") {
                let steps = if count <= 6 { 80 } else { count };
                actions.push(Action::Concurrent {
                    seed: seed.wrapping_add(iteration as u64),
                    steps: generate_steps_for(
                        Profile::Concurrent,
                        seed.wrapping_add(iteration as u64) ^ 0x5eed,
                        steps,
                    ),
                });
            }
            if matches!(scenario, "all" | "raw") {
                let steps = if count <= 6 { 100 } else { count };
                actions.push(Action::Raw {
                    seed: seed.wrapping_add(iteration as u64),
                    steps: generate_steps_for(
                        Profile::Raw,
                        seed.wrapping_add(iteration as u64) ^ 0x7a3,
                        steps,
                    ),
                });
            }
            if matches!(scenario, "all" | "scene_fuzz") {
                let cases = if count <= 6 { 40 } else { count.min(400) };
                actions.push(Action::SceneFuzz {
                    seed: seed.wrapping_add(iteration as u64),
                    cases: generate_cases(seed.wrapping_add(iteration as u64), cases),
                });
            }
            if matches!(scenario, "all" | "mouse") {
                for (mode, sgr, split) in [
                    (1002, true, false),
                    (1000, false, false),
                    (1003, true, false),
                    (1002, true, true),
                    (1000, false, true),
                ] {
                    actions.push(Action::Mouse { mode, sgr, split });
                }
            }
            if matches!(scenario, "all" | "signal") {
                for signal in [
                    FrontendSignal::Interrupt,
                    FrontendSignal::Terminate,
                    FrontendSignal::Hangup,
                ] {
                    actions.push(Action::Signal { signal });
                }
            }
            if matches!(scenario, "all" | "shutdown") {
                for mode in [
                    Shutdown::Initializing,
                    Shutdown::Lifecycle,
                    Shutdown::Output,
                ] {
                    actions.push(Action::Shutdown { mode });
                }
            }
        }
        Ok(Self {
            version: 4,
            seed,
            actions,
        })
    }
    pub fn read(path: &Path) -> Result<Self> {
        ensure(
            fs::metadata(path)?.len() <= 4 * 1024 * 1024,
            "trace exceeds 4 MiB",
        )?;
        let plan: Self = serde_json::from_slice(&fs::read(path)?)?;
        plan.validate()?;
        Ok(plan)
    }
    pub fn validate(&self) -> Result<()> {
        ensure(matches!(self.version, 1..=4), "unsupported trace version")?;
        ensure(
            !self.actions.is_empty() && self.actions.len() <= 1700,
            "invalid scenario count",
        )?;
        for action in &self.actions {
            if let Action::Paste {
                text,
                repeats,
                chunk_bytes,
                ..
            } = action
            {
                ensure(self.version >= 2, "paste actions require trace version 2")?;
                let bytes = text
                    .len()
                    .checked_mul(*repeats)
                    .ok_or("paste size overflow")?;
                ensure(
                    !text.is_empty()
                        && text.len() <= 64
                        && text.chars().all(|ch| !ch.is_control() || ch == '\n'),
                    "invalid paste text",
                )?;
                ensure(
                    bytes <= 65_537 && (1..=4096).contains(chunk_bytes),
                    "paste bounds exceeded",
                )?;
                ensure(bytes.div_ceil(*chunk_bytes) <= 256, "too many paste chunks")?;
            }
            if let Action::Signal { .. } = action {
                ensure(self.version >= 3, "signal actions require trace version 3")?;
            }
            if let Action::Copy { .. }
            | Action::History
            | Action::Zoom
            | Action::Layout
            | Action::Process = action
            {
                ensure(self.version >= 3, "this action requires trace version 3")?;
            }
            if let Action::Startup { config } = action {
                ensure(
                    *config != Config::Clipboard,
                    "startup recipes use shell configurations",
                )?;
            }
            if let Action::Mouse { mode, .. } = action {
                ensure(self.version >= 3, "mouse actions require trace version 3")?;
                ensure(
                    matches!(mode, 1000 | 1002 | 1003),
                    "unsupported mouse protocol mode",
                )?;
            }
            if let Action::Walk { steps, .. }
            | Action::Concurrent { steps, .. }
            | Action::Raw { steps, .. } = action
            {
                ensure(self.version >= 3, "walk actions require trace version 3")?;
                ensure(
                    !steps.is_empty() && steps.len() <= 5000,
                    "walk steps must be 1..5000",
                )?;
                let new = steps.iter().any(|s| {
                    !matches!(
                        s,
                        Step::Split { .. }
                            | Step::ClosePane(_)
                            | Step::CloseTab(_)
                            | Step::CloseWorkspace(_)
                            | Step::Terminate
                            | Step::Zoom
                            | Step::Rename(_)
                            | Step::Resize { .. }
                            | Step::ReorderPane(_)
                            | Step::Reorder { .. }
                            | Step::SwapDirection(_)
                            | Step::MoveDirection(_)
                            | Step::MoveTo { .. }
                            | Step::Select { .. }
                            | Step::Next { .. }
                            | Step::Previous { .. }
                            | Step::Focus(_)
                            | Step::FocusNext
                            | Step::FocusPrevious
                            | Step::FocusLast
                            | Step::FocusDirection(_)
                            | Step::Scroll(_)
                            | Step::CopyMode
                            | Step::LeaveCopyMode
                            | Step::Save
                            | Step::Load
                            | Step::Help
                            | Step::Menu
                            | Step::Choose
                            | Step::AttachViewer
                            | Step::DetachViewer
                            | Step::Key(_)
                            | Step::TabNew
                            | Step::WorkspaceNew
                    )
                });
                ensure(
                    !new || self.version >= 4,
                    "frontend, event and raw steps require trace version 4",
                )?;
                ensure(
                    matches!(action, Action::Raw { .. })
                        || !steps.iter().any(|s| matches!(s, Step::Raw { .. })),
                    "raw mutation steps belong to raw walks",
                )?;
            }
            if let Action::SceneFuzz { cases, .. } = action {
                ensure(self.version >= 4, "scene fuzz requires trace version 4")?;
                ensure(
                    !cases.is_empty() && cases.len() <= 400,
                    "scene cases must be 1..400",
                )?;
            }
            if let Action::Scale | Action::Adversarial { .. } = action {
                ensure(self.version >= 3, "this action requires trace version 3")?;
            }
            if let Action::Soak { cycles } = action {
                ensure(self.version >= 3, "soak actions require trace version 3")?;
                ensure((1..=50).contains(cycles), "soak cycles must be 1..50")?;
            }
            if let Action::Keys { sequences } = action {
                ensure(self.version >= 3, "keys actions require trace version 3")?;
                ensure(
                    !sequences.is_empty()
                        && sequences.len() <= 256
                        && sequences.iter().all(|k| !k.is_empty() && k.len() <= 16),
                    "invalid key sequences",
                )?;
            }
            if let Action::Resize { sizes, token } = action {
                ensure(
                    !sizes.is_empty() && sizes.len() <= 210,
                    "invalid resize count",
                )?;
                ensure(
                    sizes.iter().flatten().all(|n| (1..=160).contains(n)),
                    "invalid PTY size",
                )?;
                ensure(
                    !token.is_empty()
                        && token.len() <= 64
                        && token
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
                    "invalid input token",
                )?;
            }
        }
        Ok(())
    }
}

/// Every xterm encoding the frontend decodes and fux re-encodes with the same
/// bytes: an ordinary application must see what the user typed. The prefix
/// itself (Ctrl-B) is excluded because fux owns it by design.
pub fn canonical_keys() -> Vec<String> {
    let mut keys: Vec<String> = ["a", "Z", " ", "~", "界", "é"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    // Control characters: NUL, Ctrl-A..Ctrl-Z except the prefix, then the four
    // C0 controls above Ctrl-Z (Ctrl-\, Ctrl-], Ctrl-^, Ctrl-_).
    keys.push("\0".into());
    keys.extend(
        (1u8..=0x1a)
            .filter(|c| *c != 0x02 && *c != 0x1b)
            .map(|c| char::from(c).to_string()),
    );
    keys.extend((0x1cu8..=0x1f).map(|c| char::from(c).to_string()));
    keys.extend(
        [
            "\x7f",
            "\r",
            "\t",
            "\x1b[Z",
            "\x1b",
            "\x1bx",
            "\x1b[A",
            "\x1b[B",
            "\x1b[C",
            "\x1b[D",
            "\x1b[H",
            "\x1b[F",
            "\x1b[2~",
            "\x1b[3~",
            "\x1b[5~",
            "\x1b[6~",
            "\x1bOP",
            "\x1bOQ",
            "\x1bOR",
            "\x1bOS",
            "\x1b[15~",
            "\x1b[17~",
            "\x1b[24~",
            "\x1b[1;5D",
            "\x1b[1;2A",
            "\x1b[1;3C",
            "\x1b[3;5~",
            "\x1b[1;8P",
        ]
        .into_iter()
        .map(str::to_owned),
    );
    keys
}

pub struct Journal {
    file: File,
    start: Instant,
    bytes: usize,
}
impl Journal {
    pub fn new(path: &Path) -> Result<Self> {
        Ok(Self {
            file: File::create(path)?,
            start: Instant::now(),
            bytes: 0,
        })
    }
    pub fn record(&mut self, kind: &str, value: Value) -> Result<()> {
        let mut line = serde_json::to_vec(
            &json!({"ms": self.start.elapsed().as_millis(), "kind": kind, "value": value}),
        )?;
        line.push(b'\n');
        ensure(
            self.bytes + line.len() <= 64 * 1024 * 1024,
            "event journal exceeded 64 MiB",
        )?;
        self.file.write_all(&line)?;
        self.file.flush()?;
        self.bytes += line.len();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn seed_and_serialized_replay_preserve_concrete_actions() -> Result<()> {
        let plan = Plan::generate("all", 42, 2, 10)?;
        assert_eq!(plan, Plan::generate("all", 42, 2, 10)?);
        assert_ne!(plan, Plan::generate("all", 43, 2, 10)?);
        let mut replay: Plan = serde_json::from_slice(&serde_json::to_vec(&plan)?)?;
        replay.seed = 999; // informational: replay still uses the saved sizes/token
        assert_eq!(replay.actions, plan.actions);
        replay.validate()?;
        Ok(())
    }
    #[test]
    fn rejects_unbounded_or_unknown_recipes() -> Result<()> {
        let mut plan = Plan::generate("resize", 0, 1, 1)?;
        plan.version = 99;
        assert!(plan.validate().is_err());
        assert!(Plan::generate("all", 0, 101, 1).is_err());
        assert!(
            serde_json::from_str::<Action>(
                r#"{"scenario":"startup","config":"missing","extra":1}"#
            )
            .is_err()
        );
        Ok(())
    }
}
