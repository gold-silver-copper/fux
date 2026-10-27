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
    let mut admitted: Result<(), Error> = Ok(());
    let mut names = String::new();
    for root in roots {
        root.for_each_pane(&mut |id| {
            if let Some(pane) = session.parts.panes().get(&id)
                && admitted.is_ok()
            {
                admitted = (|| {
                    if !names.is_empty() {
                        budget.text(&mut names, ", ")?;
                    }
                    let label = pane.label();
                    let id = pane.id().to_string();
                    if label == id {
                        budget.text(&mut names, &id)
                    } else {
                        budget.write(&mut names, format_args!("{id} {label}"))
                    }
                })();
            }
        });
    }
    admitted?;
    Ok(names)
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
                if tab.id() == place.tab {
                    current = Some(rows.len());
                }
                let names = pane_names(session, [tab.root()], budget)?;
                let own = tab.label(session.parts.panes());
                let id = tab.id().to_string();
                let label = if own == id {
                    budget.format(format_args!("{id} — {names}"))?
                } else {
                    budget.format(format_args!("{id} {own} — {names}"))?
                };
                let bytes = names.capacity();
                drop(names);
                budget.release(bytes)?;
                budget.reserve(&mut rows, 1)?;
                rows.push((AnyRef::Tab(tab.id()), label));
            }
        }
        Choices::Workspaces => {
            let shown = session.shown_workspace(client).map(|w| w.id());
            for ws in session.parts.workspaces() {
                if Some(ws.id()) == shown {
                    current = Some(rows.len());
                }
                let roots = ws.tabs().iter().map(|t| t.root());
                let names = pane_names(session, roots, budget)?;
                let own = ws.label();
                let id = ws.id().to_string();
                let label = if own == id {
                    budget.format(format_args!("{id} — {names}"))?
                } else {
                    budget.format(format_args!("{id} {own} — {names}"))?
                };
                let bytes = names.capacity();
                drop(names);
                budget.release(bytes)?;
                budget.reserve(&mut rows, 1)?;
                rows.push((AnyRef::Workspace(ws.id()), label));
            }
        }
        Choices::Panes(source) => {
            let root = session
                .locate(source)
                .and_then(|(_, tab)| session.root(tab));
            let mut admitted: Result<(), Error> = Ok(());
            if let Some(root) = root {
                root.for_each_pane(&mut |id| {
                    if let Some(p) = session.parts.panes().get(&id).filter(|_| id != source)
                        && admitted.is_ok()
                    {
                        admitted = (|| {
                            let own = p.label();
                            let id = p.id().to_string();
                            let label = if own == id {
                                budget.string(&id)?
                            } else {
                                budget.format(format_args!("{id} {own}"))?
                            };
                            budget.reserve(&mut rows, 1)?;
                            rows.push((AnyRef::Pane(p.id()), label));
                            Ok(())
                        })();
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
