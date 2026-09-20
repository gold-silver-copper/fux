use super::*;
use vt100::{Color, Screen};

mod frontend;
mod frontend_interactions;
mod input;
mod interactions;
mod keybindings;
mod selection;

impl Server {
    fn key(&self, viewer: u64, key: &str, ctrl: bool) -> Result<(), String> {
        self.input(
            viewer,
            json!({"kind":"key","key":key,"ctrl":ctrl,"alt":false,"shift":false}),
        )
    }
    fn resize(&self, viewer: u64, rows: u16, cols: u16) -> Result<(), String> {
        self.input(viewer, json!({"kind":"resize","rows":rows,"cols":cols}))
    }
    fn painted(&self, viewer: u64, rows: u16, cols: u16) -> Result<Screen, String> {
        let frame = self.rpc("fux.frame", json!({"viewer":viewer}))?;
        let mut parser = vt100::Parser::new(rows.max(1), cols.max(1), 0);
        parser.process(frame.at("paint").as_str().need()?.as_bytes());
        Ok(parser.screen().clone())
    }
    pub(crate) fn viewer(&self, viewer: u64) -> Result<Value, String> {
        Ok(self
            .query("fux::model::Viewer")?
            .rows()
            .find(|r| r.at("entity") == viewer)
            .need()?
            .at("components")
            .at("fux::model::Viewer"))
    }
    /// Whether the prefix command column is painted for this viewer.
    fn column_open(&self, viewer: u64, rows: u16, cols: u16) -> Result<bool, String> {
        Ok(self
            .painted(viewer, rows, cols)?
            .contents()
            .contains("Commands"))
    }
    /// The reversed (selected) row of the painted command column, if it is open.
    fn selected(&self, viewer: u64, rows: u16, cols: u16) -> Result<Option<String>, String> {
        let screen = self.painted(viewer, rows, cols)?;
        if !screen.contents().contains("Commands") {
            return Ok(None);
        }
        // The bar's active tab is also reversed, so only rows above it count, and
        // only the reversed cells: pane content shares the row left of the column.
        Ok((0..rows.saturating_sub(1))
            .find(|&y| (0..cols).any(|x| screen.cell(y, x).is_some_and(|c| c.inverse())))
            .map(|y| {
                (0..cols)
                    .filter_map(|x| screen.cell(y, x).filter(|c| c.inverse()))
                    .map(vt100::Cell::contents)
                    .collect::<String>()
                    .trim()
                    .to_owned()
            }))
    }
    fn run(&self, viewer: u64, command: &str) -> Result<(), String> {
        self.input(viewer, json!({"kind":"paste","text":command}))?;
        self.enter(viewer)
    }
    fn mouse(&self, viewer: u64, action: &str, x: u16, y: u16) -> Result<(), String> {
        self.input(viewer, json!({"kind":"mouse","action":action,"button":"left","x":x,"y":y,"ctrl":false,"alt":false,"shift":false}))
    }
    fn capture(&self, viewer: u64, rows: u16, cols: u16, name: &str) -> Result<(), Fail> {
        if let Ok(directory) = std::env::var("FUX_DESIGN_CAPTURE") {
            let directory = PathBuf::from(directory);
            fs::create_dir_all(&directory)?;
            let frame = self.rpc("fux.frame", json!({"viewer":viewer}))?;
            let paint = frame.at("paint");
            let paint = paint.as_str().need()?;
            fs::write(directory.join(format!("{name}.ansi")), paint)?;
            let mut parser = vt100::Parser::new(rows.max(1), cols.max(1), 0);
            parser.process(paint.as_bytes());
            let screen = parser.screen();
            fs::write(directory.join(format!("{name}.txt")), plain(screen))?;
            let cells: Vec<_> = (0..rows).map(|y| (0..cols).map(|x| {
                screen.cell(y,x).map_or(Value::Null, |c| json!({"text":c.contents(),"fg":format!("{:?}",c.fgcolor()),"bg":format!("{:?}",c.bgcolor()),"bold":c.bold(),"dim":c.dim(),"inverse":c.inverse(),"wide_continuation":c.is_wide_continuation()}))
            }).collect::<Vec<_>>()).collect();
            fs::write(
                directory.join(format!("{name}.json")),
                serde_json::to_vec(
                    &json!({"rows":rows,"cols":cols,"cells":cells,"cursor":screen.cursor_position(),"hidden":screen.hide_cursor()}),
                )?,
            )?;
        }
        Ok(())
    }
}

fn plain(screen: &Screen) -> String {
    screen
        .contents()
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}

fn text(screen: &Screen, y: u16, x: u16) -> &str {
    screen.cell(y, x).map_or("", vt100::Cell::contents)
}
fn row(screen: &Screen, y: u16) -> String {
    (0..screen.size().1).map(|x| text(screen, y, x)).collect()
}

#[test]
fn borderless_surface_bottom_bar_edges_and_tiny_viewports() -> Outcome {
    let s = Server::start()?;
    let v = s.attach()?;
    s.screen(v)?; // negotiate the PTY before positioning output
    s.run(
        v,
        r#"exec /bin/sh -c 'printf "\033[2J\033[H\033[31;44mEDGE\033[23;80HZ\033[5;6H"; exec sleep 60'"#,
    )?;
    eventually(|| {
        let screen = s.painted(v, 24, 80)?;
        Ok(text(&screen, 22, 79) == "Z" && screen.cursor_position() == (4, 5))
    })?;
    let screen = s.painted(v, 24, 80)?;
    assert_eq!(&row(&screen, 0)[..4], "EDGE");
    assert_eq!(screen.cursor_position(), (4, 5));
    assert!(!screen.hide_cursor());
    assert_eq!(screen.cell(0, 0).need()?.fgcolor(), Color::Idx(1));
    assert_eq!(screen.cell(0, 0).need()?.bgcolor(), Color::Idx(4));
    assert_eq!(screen.cell(22, 79).need()?.bgcolor(), Color::Idx(4));
    for col in 0..80 {
        assert_eq!(screen.cell(23, col).need()?.bgcolor(), Color::Idx(8));
    }
    assert!(row(&screen, 23).starts_with(" main"));
    assert!(!screen.contents().contains('+'));
    s.capture(v, 24, 80, "single")?;
    for (rows, cols) in [(1, 80), (2, 1), (1, 1), (0, 0), (24, 80)] {
        s.resize(v, rows, cols)?;
        let screen = s.painted(v, rows, cols)?;
        if rows == 1 && cols > 0 {
            assert_eq!(screen.cell(0, cols - 1).need()?.bgcolor(), Color::Idx(8));
            assert!(screen.hide_cursor());
        }
        if rows == 0 {
            assert_eq!(screen.contents(), "");
        }
    }
    s.resize(v, 1, 20)?;
    s.capture(v, 1, 20, "tiny")?;
    Ok(())
}

#[test]
fn native_splits_junctions_focus_zoom_and_no_margin_chrome() -> Outcome {
    let s = Server::start()?;
    let v = s.attach()?;
    s.resize(v, 15, 61)?;
    s.split(v, "horizontal", None)?;
    s.split(v, "vertical", None)?;
    let screen = s.painted(v, 15, 61)?;
    // Native layout gives left width 30, right width 30 and one shared gap.
    assert_eq!(text(&screen, 0, 30), "│");
    assert_eq!(text(&screen, 7, 30), "├");
    assert_eq!(text(&screen, 7, 31), "─");
    assert_eq!(text(&screen, 7, 60), "─");
    assert!(screen.cell(10, 30).need()?.bold());
    assert_eq!(screen.cell(0, 30).need()?.fgcolor(), Color::Idx(8));
    s.capture(v, 15, 61, "nested")?;
    let before = s.focused(v)?;
    s.mouse(v, "press", 0, 0)?;
    assert_ne!(s.focused(v)?, before);
    let screen = s.painted(v, 15, 61)?;
    assert!(screen.cell(0, 30).need()?.bold());
    assert!(!screen.cell(7, 50).need()?.bold());
    let selected = s.focused(v)?;
    s.mouse(v, "press", 30, 3)?; // separator cannot pick a pane
    s.mouse(v, "press", 0, 14)?; // bottom bar cannot pick a pane
    assert_eq!(s.focused(v)?, selected);
    s.command(v, "zoom")?;
    let screen = s.painted(v, 15, 61)?;
    assert!(!screen.contents().contains('│') || row(&screen, 14).contains('│'));
    assert_eq!(text(&screen, 0, 30), " ");
    assert_eq!(text(&screen, 7, 31), " ");
    s.command(v, "zoom")?;
    s.painted(v, 15, 61)?;
    // Preserve arbitrary native spacing instead of painting every empty cell.
    let nodes = s.query("bevy_ui::ui_node::Node")?;
    for split in s.query("fux::model::Split")?.rows() {
        let mut node = nodes
            .rows()
            .find(|n| n.at("entity") == split.at("entity"))
            .need()?
            .at("components")
            .at("bevy_ui::ui_node::Node")
            .clone();
        let fields = node.as_object_mut().need()?;
        fields.insert("column_gap".into(), json!({"Px":5.0}));
        fields.insert("row_gap".into(), json!({"Px":5.0}));
        s.rpc(
            "world.insert_components",
            json!({"entity":split.at("entity"),"components":{"bevy_ui::ui_node::Node":node}}),
        )?;
    }
    let screen = s.painted(v, 15, 61)?;
    for y in 0..14 {
        assert!(!row(&screen, y).contains(['│', '─', '├']));
    }
    Ok(())
}

#[test]
fn command_column_prefix_policy_scroll_prompts_and_repaint() -> Outcome {
    let s = Server::start()?;
    let v = s.attach()?;
    s.resize(v, 12, 60)?;
    let live = s.painted(v, 12, 60)?;
    s.key(v, "b", true)?;
    let panel = s.painted(v, 12, 60)?;
    assert!(panel.hide_cursor());
    assert!(panel.contents().contains("Commands"));
    assert!(panel.contents().contains("split side by side"));
    assert!(panel.contents().contains("▼"));
    assert_eq!(panel.cell(10, 59).need()?.bgcolor(), Color::Idx(8));
    assert!(row(&panel, 11).starts_with(" main"));
    assert_eq!(panel.cell(0, 0).need()?.bgcolor(), Color::Default);
    s.key(v, "f12", false)?;
    assert!(s.column_open(v, 12, 60)?);
    s.input(v, json!({"kind":"paste","text":"NOT-PTY-INPUT"}))?;
    s.key(v, "escape", false)?;
    let closed = s.painted(v, 12, 60)?;
    assert_eq!(closed.hide_cursor(), live.hide_cursor());
    assert!(!closed.contents().contains("Commands"));
    assert!(!closed.contents().contains("NOT-PTY-INPUT"));
    // Modified arrow invokes resize; unmodified arrows belong to the list.
    let focus = s.focused(v)?;
    let grow = || -> Result<f64, String> {
        s.query("bevy_ui::ui_node::Node")?
            .rows()
            .find(|n| n.at("entity") == focus)
            .need()?
            .at("components")
            .at("bevy_ui::ui_node::Node")
            .at("flex_grow")
            .as_f64()
            .need()
    };
    let before = grow()?;
    s.key(v, "b", true)?;
    s.key(v, "down", false)?;
    assert_eq!(grow()?, before);
    assert_eq!(s.selected(v, 12, 60)?.as_deref(), Some("v  split stacked"));
    s.key(v, "right", true)?;
    assert!(grow()? > before);
    assert!(!s.column_open(v, 12, 60)?);
    // Help is the command column itself: the prefix key is the only way in.
    s.key(v, "b", true)?;
    s.painted(v, 12, 60)?;
    s.mouse(v, "scroll_down", 59, 10)?;
    assert_eq!(s.selected(v, 12, 60)?.as_deref(), Some("v  split stacked"));
    s.mouse(v, "scroll_up", 59, 10)?;
    assert_eq!(
        s.selected(v, 12, 60)?.as_deref(),
        Some("h  split side by side")
    );
    s.key(v, "down", false)?;
    assert_eq!(s.selected(v, 12, 60)?.as_deref(), Some("v  split stacked"));
    s.key(v, "pagedown", false)?;
    let paged = s.selected(v, 12, 60)?.need()?;
    assert!(!paged.contains("split"), "{paged}");
    let screen = s.painted(v, 12, 60)?;
    assert!(screen.contents().contains("▲"));
    let focus = s.focused(v)?;
    s.mouse(v, "press", 0, 0)?;
    assert_eq!(s.focused(v)?, focus);
    s.input(v, json!({"kind":"paste","text":"NOT-HELP-INPUT"}))?;
    assert!(!s.painted(v, 12, 60)?.contents().contains("NOT-HELP-INPUT"));
    for _ in 0..60 {
        s.key(v, "down", false)?;
    }
    assert!(s.painted(v, 12, 60)?.contents().contains("d  detach"));
    s.resize(v, 60, 60)?;
    assert!(
        s.painted(v, 60, 60)?
            .contents()
            .contains("split side by side")
    );
    s.key(v, "home", false)?;
    assert_eq!(
        s.selected(v, 60, 60)?.as_deref(),
        Some("h  split side by side")
    );
    s.capture(v, 60, 60, "help")?;
    s.resize(v, 7, 18)?;
    s.capture(v, 7, 18, "narrow-help")?;
    s.key(v, "escape", false)?;
    s.resize(v, 12, 60)?;
    s.key(v, "b", true)?;
    s.key(v, "r", false)?;
    s.input(v, json!({"kind":"paste","text":"界é-long-name"}))?;
    s.key(v, "backspace", false)?;
    let screen = s.painted(v, 12, 60)?;
    assert!(screen.contents().contains("rename pane"));
    assert!(screen.contents().contains("界é-long-nam▏"));
    assert!(screen.hide_cursor());
    let input_row = 9;
    assert!((0..60).any(|x| screen.cell(input_row, x).is_some_and(|c| c.inverse())));
    s.capture(v, 12, 60, "prompt")?;
    s.key(v, "escape", false)?;
    assert!(!s.painted(v, 12, 60)?.contents().contains("rename pane"));
    s.key(v, "b", true)?;
    s.key(v, "r", false)?;
    s.input(v, json!({"kind":"paste","text":"renamed"}))?;
    s.enter(v)?;
    assert!(row(&s.painted(v, 12, 60)?, 11).contains("renamed"));
    Ok(())
}
