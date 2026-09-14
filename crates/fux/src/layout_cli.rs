//! CLI for existing-pane layout edits. Exported revisions make concurrent edits explicit.
use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use fux::ids::{PaneId, TabId};
use fux::layout::{Direction, LayoutDocument, NodeId};
use fux::proto::control::{
    CommandResult, LayoutAction, LayoutZoom, MAX_FRAME_BYTES, PaneDestination, Reply, Request,
};
use std::io::Read;
use std::path::PathBuf;

#[derive(Debug, Args)]
pub struct LayoutArgs {
    /// Stable tab ID from `fux list`.
    tab: u32,
    /// Observed layout generation; required for changes.
    #[arg(long, global = true)]
    generation: Option<u64>,
    /// Observed server instance from layout export; required for changes.
    #[arg(long, global = true)]
    instance: Option<String>,
    #[command(subcommand)]
    action: Edit,
}

#[derive(Debug, Default, Args)]
struct TransferFocus {
    /// Select the moved pane in the destination workspace default.
    #[arg(long, conflicts_with = "no_focus")]
    focus: bool,
    /// Preserve existing selections (default); source removal may require a fallback.
    #[arg(long)]
    no_focus: bool,
}

#[derive(Debug, Args)]
pub struct WorkspaceTransferArgs {
    #[command(flatten)]
    focus: TransferFocus,
    /// Existing live pane to transfer using the global manager socket.
    pane: u32,
    #[arg(long)]
    instance: String,
    #[arg(long)]
    source_tab: u32,
    #[arg(long)]
    generation: u64,
    /// Destination workspace name and lifetime from its listing.
    #[arg(long)]
    workspace: String,
    #[arg(
        long,
        required_unless_present = "new_workspace",
        conflicts_with = "new_workspace"
    )]
    stream: Option<u64>,
    /// Create the destination workspace around this pane, without starting another process.
    #[arg(long, conflicts_with = "destination_tab")]
    new_workspace: bool,
    /// Omit to create a new tab around the existing pane.
    #[arg(long, requires_all = ["destination_generation", "target"], conflicts_with = "label")]
    destination_tab: Option<u32>,
    #[arg(long, requires = "destination_tab")]
    destination_generation: Option<u64>,
    #[arg(long, requires = "destination_tab")]
    target: Option<u32>,
    #[arg(long, value_enum, default_value = "right")]
    side: Direction,
    /// Existing target's share on a 10000 scale; only applies to an existing tab.
    #[arg(long, requires = "destination_tab", value_parser = clap::value_parser!(u16).range(500..=9500))]
    ratio: Option<u16>,
    /// Label for a new destination tab.
    #[arg(long)]
    label: Option<String>,
}

impl WorkspaceTransferArgs {
    pub fn request(self) -> Result<fux::daemon::ManagerRequest> {
        let destination = match (
            self.destination_tab,
            self.destination_generation,
            self.target,
        ) {
            (Some(tab), Some(generation), Some(target)) => PaneDestination::Tab {
                ratio: self.ratio.unwrap_or(5000),
                tab: TabId(tab),
                generation,
                target: PaneId(target),
            },
            (None, None, None) => PaneDestination::NewTab { label: self.label },
            _ => bail!(
                "existing destination needs --destination-tab, --destination-generation and --target"
            ),
        };
        Ok(fux::daemon::ManagerRequest::Transfer {
            transfer: fux::proto::control::WorkspaceTransfer {
                focus: self.focus.focus,
                follow: None,
                instance: self.instance,
                source: TabId(self.source_tab),
                generation: self.generation,
                pane: PaneId(self.pane),
                workspace: if self.new_workspace {
                    fux::proto::control::WorkspaceDestination::New {
                        name: self.workspace,
                    }
                } else {
                    fux::proto::control::WorkspaceDestination::Existing {
                        name: self.workspace,
                        stream: self.stream.context("destination stream required")?,
                    }
                },
                destination,
                side: self.side,
            },
        })
    }
}

#[derive(Debug, Subcommand)]
enum Edit {
    /// Move the existing pane to a new tab in this workspace.
    NewTab {
        pane: u32,
        label: Option<String>,
        #[command(flatten)]
        focus: TransferFocus,
    },
    /// Move beside a pane in another tab using its observed generation.
    ToTab {
        pane: u32,
        #[command(flatten)]
        focus: TransferFocus,
        tab: u32,
        destination_generation: u64,
        target: u32,
        #[arg(value_enum)]
        side: Direction,
        /// Existing target pane share on a 10000 scale.
        #[arg(long, default_value_t = 5000, value_parser = clap::value_parser!(u16).range(500..=9500))]
        ratio: u16,
    },
    /// Read pane geometry, directional neighbors and outer-edge flags without changing focus.
    Inspect { pane: u32 },
    /// Export geometry, existing pane IDs and the revision for later edits.
    Export,
    /// Exchange two panes' positions without changing their processes.
    Swap { pane: u32, target: u32 },
    /// Remove/reinsert a pane beside another pane in this tab.
    Move {
        pane: u32,
        target: u32,
        #[arg(value_enum)]
        side: Direction,
    },
    /// Grow the branch toward a directional boundary (negative delta shrinks).
    Resize {
        pane: u32,
        #[arg(value_enum)]
        direction: Direction,
        #[arg(allow_hyphen_values = true)]
        delta: i16,
    },
    /// Set an exported split node's ratio in units of 1/10,000 (500..9500).
    Ratio { split: u32, ratio: u16 },
    /// Show one pane at full size; omit PANE to restore the split layout.
    Zoom { pane: Option<u32> },
    /// Apply a document or complete export reply from FILE to these existing panes.
    Apply {
        file: PathBuf,
        /// Complete source-to-existing-pane mapping; repeat for every source pane.
        #[arg(long = "map", value_parser = parse_mapping)]
        remap: Vec<(PaneId, PaneId)>,
    },
}

impl LayoutArgs {
    pub fn request(self) -> Result<Request> {
        if !matches!(self.action, Edit::Export | Edit::Inspect { .. })
            && (self.generation.is_none() || self.instance.is_none())
        {
            bail!("export the layout first; changes require --generation and --instance");
        }
        let action = match self.action {
            Edit::Export => LayoutAction::Export,
            Edit::Inspect { pane } => LayoutAction::Inspect { pane: PaneId(pane) },
            Edit::NewTab { pane, label, focus } => LayoutAction::Transfer {
                focus: focus.focus,
                pane: PaneId(pane),
                destination: PaneDestination::NewTab { label },
                side: Direction::Right,
            },
            Edit::ToTab {
                focus,
                pane,
                tab,
                destination_generation,
                target,
                side,
                ratio,
            } => LayoutAction::Transfer {
                focus: focus.focus,
                pane: PaneId(pane),
                destination: PaneDestination::Tab {
                    ratio,
                    tab: TabId(tab),
                    generation: destination_generation,
                    target: PaneId(target),
                },
                side,
            },
            Edit::Swap { pane, target } => LayoutAction::Swap {
                pane: PaneId(pane),
                target: PaneId(target),
            },
            Edit::Move { pane, target, side } => LayoutAction::Relocate {
                pane: PaneId(pane),
                target: PaneId(target),
                side,
            },
            Edit::Resize {
                pane,
                direction,
                delta,
            } => LayoutAction::ResizeToward {
                pane: PaneId(pane),
                direction,
                delta,
            },
            Edit::Ratio { split, ratio } => LayoutAction::SetRatio {
                split: NodeId(split),
                ratio,
            },
            Edit::Zoom { pane } => LayoutAction::Zoom {
                pane: pane.map(PaneId),
            },
            Edit::Apply { file, remap } => {
                let (document, zoom, labels) = read_document(&file)?;
                LayoutAction::Apply {
                    document,
                    remap,
                    zoom,
                    labels,
                }
            }
        };
        let request = Request::Layout {
            id: 1,
            instance: self.instance,
            tab: TabId(self.tab),
            generation: self.generation,
            action,
        };
        request.validate()?;
        Ok(request)
    }
}

fn parse_mapping(value: &str) -> Result<(PaneId, PaneId), String> {
    let (source, destination) = value
        .split_once('=')
        .ok_or_else(|| "use SOURCE=DESTINATION".to_owned())?;
    Ok((
        PaneId(source.parse().map_err(|_| "invalid source pane ID")?),
        PaneId(
            destination
                .parse()
                .map_err(|_| "invalid destination pane ID")?,
        ),
    ))
}

type ImportedLayout = (
    LayoutDocument<PaneId>,
    LayoutZoom,
    Option<fux::proto::control::PaneLabels>,
);

fn read_document(path: &std::path::Path) -> Result<ImportedLayout> {
    let file =
        std::fs::File::open(path).with_context(|| format!("open layout {}", path.display()))?;
    let mut bytes = Vec::new();
    file.take((MAX_FRAME_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_FRAME_BYTES {
        bail!("layout exceeds the control frame size limit");
    }
    parse_document(&bytes)
}

fn parse_document(bytes: &[u8]) -> Result<ImportedLayout> {
    if let Ok(document) = serde_json::from_slice::<LayoutDocument<PaneId>>(bytes) {
        return Ok((document, LayoutZoom::Preserve, None));
    }
    match serde_json::from_slice::<Reply>(bytes)
        .context("expected a layout document or layout export reply")?
    {
        Reply::Completed {
            result:
                CommandResult::Layout {
                    document,
                    zoomed,
                    labels,
                    ..
                },
            ..
        } => Ok((document, LayoutZoom::Set { pane: zoomed }, Some(labels))),
        _ => bail!("file is not a successful layout export"),
    }
}

pub fn read_archive(path: &std::path::Path) -> Result<fux::proto::control::LayoutArchive> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take((MAX_FRAME_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_FRAME_BYTES {
        bail!("archive exceeds the control frame size limit");
    }
    if let Ok(archive) = serde_json::from_slice::<fux::proto::control::LayoutArchive>(&bytes) {
        return Ok(archive);
    }
    match serde_json::from_slice::<fux::daemon::ManagerReply>(&bytes)? {
        fux::daemon::ManagerReply::LayoutArchive { archive } => Ok(archive),
        _ => bail!("file is not a layout archive"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        layout: LayoutArgs,
    }

    #[derive(Parser)]
    struct TransferCli {
        #[command(flatten)]
        transfer: WorkspaceTransferArgs,
    }

    #[test]
    fn creating_a_destination_is_explicit_and_incompatible_with_existing_targets() -> Result<()> {
        let base = [
            "transfer",
            "1",
            "--instance",
            "test-instance",
            "--source-tab",
            "1",
            "--generation",
            "7",
            "--workspace",
            "new",
        ];
        assert!(TransferCli::try_parse_from(base).is_err());
        assert!(
            TransferCli::try_parse_from(base.into_iter().chain([
                "--new-workspace",
                "--focus",
                "--no-focus"
            ]))
            .is_err()
        );
        let focused =
            TransferCli::try_parse_from(base.into_iter().chain(["--new-workspace", "--focus"]))?;
        assert!(matches!(
            focused.transfer.request()?,
            fux::daemon::ManagerRequest::Transfer {
                transfer: fux::proto::control::WorkspaceTransfer { focus: true, .. }
            }
        ));

        let create = TransferCli::try_parse_from(base.into_iter().chain(["--new-workspace"]))?;
        assert!(matches!(
            create.transfer.request()?,
            fux::daemon::ManagerRequest::Transfer {
                transfer: fux::proto::control::WorkspaceTransfer {
                    focus: false,
                    workspace: fux::proto::control::WorkspaceDestination::New { .. },
                    destination: PaneDestination::NewTab { .. },
                    ..
                }
            }
        ));
        assert!(
            TransferCli::try_parse_from(base.into_iter().chain([
                "--new-workspace",
                "--stream",
                "4"
            ]))
            .is_err()
        );
        assert!(
            TransferCli::try_parse_from(base.into_iter().chain([
                "--new-workspace",
                "--destination-tab",
                "2",
                "--destination-generation",
                "8",
                "--target",
                "3"
            ]))
            .is_err()
        );
        assert!(
            TransferCli::try_parse_from(base.into_iter().chain([
                "--new-workspace",
                "--ratio",
                "7000"
            ]))
            .is_err()
        );
        for ratio in ["499", "9501", "nan"] {
            assert!(
                TransferCli::try_parse_from(base.into_iter().chain([
                    "--stream",
                    "4",
                    "--destination-tab",
                    "2",
                    "--destination-generation",
                    "8",
                    "--target",
                    "3",
                    "--ratio",
                    ratio
                ]))
                .is_err()
            );
        }
        let configured = TransferCli::try_parse_from(base.into_iter().chain([
            "--stream",
            "4",
            "--destination-tab",
            "2",
            "--destination-generation",
            "8",
            "--target",
            "3",
            "--ratio",
            "7000",
        ]))?;
        assert!(matches!(
            configured.transfer.request()?,
            fux::daemon::ManagerRequest::Transfer {
                transfer: fux::proto::control::WorkspaceTransfer {
                    focus: false,
                    destination: PaneDestination::Tab { ratio: 7000, .. },
                    ..
                }
            }
        ));
        let existing = TransferCli::try_parse_from(base.into_iter().chain(["--stream", "4"]))?;
        assert!(matches!(
            existing.transfer.request()?,
            fux::daemon::ManagerRequest::Transfer {
                transfer: fux::proto::control::WorkspaceTransfer {
                    focus: false,
                    workspace: fux::proto::control::WorkspaceDestination::Existing {
                        stream: 4,
                        ..
                    },
                    ..
                }
            }
        ));
        Ok(())
    }

    #[test]
    fn full_exports_restore_zoom_but_bare_documents_preserve_it() -> Result<()> {
        let document = LayoutDocument {
            root: Some(0),
            nodes: vec![fux::layout::LayoutNode::Pane { pane: PaneId(7) }],
        };
        assert_eq!(
            parse_document(&serde_json::to_vec(&document)?)?,
            (document.clone(), LayoutZoom::Preserve, None)
        );
        for zoomed in [None, Some(PaneId(7))] {
            let reply = Reply::Completed {
                id: 1,
                result: CommandResult::Layout {
                    instance: "source".into(),
                    tab: TabId(3),
                    generation: 9,
                    document: document.clone(),
                    zoomed,
                    labels: vec![(PaneId(7), "saved label".into())],
                },
            };
            assert_eq!(
                parse_document(&serde_json::to_vec(&reply)?)?,
                (
                    document.clone(),
                    LayoutZoom::Set { pane: zoomed },
                    Some(vec![(PaneId(7), "saved label".into())])
                )
            );
        }
        Ok(())
    }

    #[test]
    fn changes_require_observed_identity_and_export_needs_no_guard() -> Result<()> {
        let export = TestCli::try_parse_from(["layout", "1", "export"])?;
        assert!(matches!(
            export.layout.request()?,
            Request::Layout {
                action: LayoutAction::Export,
                generation: None,
                ..
            }
        ));
        let inspect = TestCli::try_parse_from(["layout", "1", "inspect", "2"])?;
        assert!(matches!(
            inspect.layout.request()?,
            Request::Layout {
                action: LayoutAction::Inspect { pane: PaneId(2) },
                generation: None,
                instance: None,
                ..
            }
        ));
        let missing = TestCli::try_parse_from(["layout", "1", "swap", "1", "2"])?;
        assert!(missing.layout.request().is_err());
        let resize = TestCli::try_parse_from([
            "layout",
            "1",
            "--generation",
            "7",
            "--instance",
            "test-instance",
            "resize",
            "2",
            "left",
            "-500",
        ])?;
        assert!(matches!(
            resize.layout.request()?,
            Request::Layout {
                action: LayoutAction::ResizeToward {
                    delta: -500,
                    direction: Direction::Left,
                    ..
                },
                generation: Some(7),
                ..
            }
        ));
        assert!(TestCli::try_parse_from(["layout", "1", "move", "1", "2", "diagonal"]).is_err());
        Ok(())
    }
}
