//! What the server does for a client's own screens, which the client owns
//! (`crate::screens`): the rows of a chooser, made of what only the server
//! knows; and the room a panel has on a screen of a size.
use crate::command::{AnyRef, Choices, Kind};
use crate::id::ClientId;
use crate::layout::Tree;
use crate::protocol::{Body, Reply};
use crate::session::{Error, Session};

/// The names of the panes in `roots`, for a chooser row, shortened.
fn pane_names<'a>(
    session: &Session,
    roots: impl IntoIterator<Item = &'a Tree>,
    budget: &mut crate::wire::Budget,
) -> Result<String, Error> {
    let mut admitted = Ok(());
    let mut names: Vec<String> = Vec::new();
    for root in roots {
        root.for_each_pane(&mut |id| {
            if let Some(p) = session.parts.panes().get(&id)
                && admitted.is_ok()
            {
                admitted = budget.charge(p.label().len().saturating_add(64).saturating_mul(2));
                if admitted.is_ok() {
                    names.push(crate::words::labelled(p.id(), &p.label()));
                }
            }
        });
    }
    admitted?;
    Ok(names.join(", "))
}

/// A chooser's rows for client `client`: what each is, its label, and
/// which one is the client's own.
pub fn choices(session: &Session, client: ClientId, choices: Choices) -> Result<Reply, Error> {
    let budget = &mut crate::wire::Budget::new();
    let mut rows = Vec::new();
    let mut current = None;
    match choices {
        Choices::Tabs => {
            let place = session.place(client).ok_or(Error::NoClient(client))?;
            let ws = session
                .workspace(place.ws)
                .ok_or(Error::NoWorkspace(place.ws))?;
            for tab in ws.tabs() {
                budget.charge(tab.label(session.parts.panes()).len().saturating_add(128))?;
                if tab.id() == place.tab {
                    current = Some(rows.len());
                }
                let label = format!(
                    "{} — {}",
                    crate::words::labelled(tab.id(), &tab.label(session.parts.panes())),
                    pane_names(session, [tab.root()], budget)?
                );
                rows.push((AnyRef::Tab(tab.id()), label));
            }
        }
        Choices::Workspaces => {
            let shown = session.shown_workspace(client).map(|w| w.id());
            for ws in session.parts.workspaces() {
                budget.charge(ws.label().len().saturating_add(128))?;
                if Some(ws.id()) == shown {
                    current = Some(rows.len());
                }
                let roots = ws.tabs().iter().map(|t| t.root());
                let label = format!(
                    "{} — {}",
                    crate::words::labelled(ws.id(), &ws.label()),
                    pane_names(session, roots, budget)?
                );
                rows.push((AnyRef::Workspace(ws.id()), label));
            }
        }
        Choices::Panes(source) => {
            let root = session
                .locate(source)
                .and_then(|(_, tab)| session.root(tab));
            let mut admitted = Ok(());
            if let Some(root) = root {
                root.for_each_pane(&mut |id| {
                    if let Some(p) = session.parts.panes().get(&id).filter(|_| id != source)
                        && admitted.is_ok()
                    {
                        admitted = budget.charge(p.label().len().saturating_add(128));
                        if admitted.is_ok() {
                            rows.push((
                                AnyRef::Pane(p.id()),
                                crate::words::labelled(p.id(), &p.label()),
                            ));
                        }
                    }
                });
            }
            admitted?;
            if rows.is_empty() {
                return Err(Error::OnlyOne(Kind::Pane));
            }
        }
    }
    Ok(Body::Choices { rows, current }.into())
}

/// Rows a scrolled panel has for its entries on a screen of `rows`: the
/// rows above the bar, less `fixed` lines of its own (a title, a help line)
/// and the two lines that say how many entries lie above and below; at
/// least one.
pub fn panel_room(rows: u16, fixed: usize) -> usize {
    usize::from(rows.saturating_sub(1))
        .saturating_sub(fixed.saturating_add(2))
        .max(1)
}

/// The first of `len` entries a panel with `room` rows for them shows, so
/// that `selected` is in view: the window ends at the selection, or at the
/// last entry.
pub fn window_start(len: usize, selected: usize, room: usize) -> usize {
    selected
        .saturating_add(1)
        .saturating_sub(room)
        .min(len.saturating_sub(room))
}
