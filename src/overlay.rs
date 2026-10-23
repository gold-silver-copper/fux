//! What the server does for a client's own screens, which the client owns
//! (`crate::screens`): the rows of a chooser, made of what only the server
//! knows; and the room a panel has on a screen of a size.
use crate::command::{AnyRef, Choices, Kind};
use crate::id::ClientId;
use crate::layout::Layout;
use crate::protocol::{Reply, Scalar};
use crate::session::{Error, Session};

/// The names of the panes in `layouts`, for a chooser row, shortened.
fn pane_names<'a>(session: &Session, layouts: impl IntoIterator<Item = &'a Layout>) -> String {
    let panes = layouts.into_iter().flat_map(Layout::panes);
    let names: Vec<String> = panes
        .filter_map(|id| session.panes.get(&id))
        .map(|p| format!("{} {}", p.id, p.label()))
        .collect();
    if names.is_empty() {
        "empty".into()
    } else {
        names.join(", ")
    }
}

/// A chooser's rows for client `client`: what each is (its ids), its label
/// (a line of text each), and which one is the client's own (its value).
pub fn choices(session: &Session, client: ClientId, choices: Choices) -> Result<Reply, Error> {
    let mut rows = Vec::new();
    let mut current = None;
    match choices {
        Choices::Tabs => {
            let place = session.place(client).ok_or(Error::NoClient(client))?;
            let ws = session
                .workspace(place.ws)
                .ok_or(Error::NoWorkspace(place.ws))?;
            for tab in ws.tabs() {
                if tab.id == place.tab {
                    current = Some(rows.len());
                }
                let label = format!(
                    "{} {} — {}",
                    tab.id,
                    tab.name,
                    pane_names(session, [tab.layout()])
                );
                rows.push((AnyRef::Tab(tab.id), label));
            }
        }
        Choices::Workspaces => {
            let shown = session.shown_workspace(client).map(|w| w.id);
            for ws in &session.workspaces {
                if Some(ws.id) == shown {
                    current = Some(rows.len());
                }
                let layouts = ws.tabs().iter().map(|t| t.layout());
                let label = format!("{} {} — {}", ws.id, ws.name, pane_names(session, layouts));
                rows.push((AnyRef::Workspace(ws.id), label));
            }
        }
        Choices::Panes(source) => {
            let tab = session.locate(source).and_then(|(_, tab)| session.tab(tab));
            let others = tab.into_iter().flat_map(|t| t.layout().panes());
            for p in others
                .filter(|id| *id != source)
                .filter_map(|id| session.panes.get(&id))
            {
                rows.push((AnyRef::Pane(p.id), format!("{} {}", p.id, p.label())));
            }
            if rows.is_empty() {
                return Err(Error::OnlyOne(Kind::Pane));
            }
        }
    }
    let (ids, labels): (_, Vec<String>) = rows.into_iter().unzip();
    Ok(Reply {
        ids,
        text: labels.iter().map(|l| format!("{l}\n")).collect(),
        value: (current.and_then(|i| i64::try_from(i).ok())).map(Scalar::Int),
        ..Reply::default()
    })
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
