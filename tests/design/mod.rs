use super::*;
use vt100::{Color, Screen};

mod frontend;
mod frontend_interactions;
mod input;
mod interactions;
mod keybindings;
mod selection;

impl Server {
    fn key(&self, viewer: u64, key: &str, ctrl: bool) {
        self.input(
            viewer,
            json!({"kind":"key","key":key,"ctrl":ctrl,"alt":false,"shift":false}),
        );
    }
    fn resize(&self, viewer: u64, rows: u16, cols: u16) {
        self.input(viewer, json!({"kind":"resize","rows":rows,"cols":cols}));
    }
    fn painted(&self, viewer: u64, rows: u16, cols: u16) -> Screen {
        let frame = self.rpc("fux.frame", json!({"viewer":viewer}));
        let mut parser = vt100::Parser::new(rows.max(1), cols.max(1), 0);
        parser.process(frame["paint"].as_str().unwrap().as_bytes());
        parser.screen().clone()
    }
    fn viewer(&self, viewer: u64) -> Value {
        self.query("fux::model::Viewer")
            .into_iter()
            .find(|r| r["entity"] == viewer)
            .unwrap()["components"]["fux::model::Viewer"]
            .clone()
    }
    fn run(&self, viewer: u64, command: &str) {
        self.input(viewer, json!({"kind":"paste","text":command}));
        self.enter(viewer);
    }
    fn mouse(&self, viewer: u64, action: &str, x: u16, y: u16) {
        self.input(viewer, json!({"kind":"mouse","action":action,"button":0,"x":x,"y":y,"ctrl":false,"alt":false,"shift":false}));
    }
    fn capture(&self, viewer: u64, rows: u16, cols: u16, name: &str) {
        if let Ok(directory) = std::env::var("FUX_DESIGN_CAPTURE") {
            let directory = PathBuf::from(directory);
            fs::create_dir_all(&directory).unwrap();
            let frame = self.rpc("fux.frame", json!({"viewer":viewer}));
            let paint = frame["paint"].as_str().unwrap();
            fs::write(directory.join(format!("{name}.ansi")), paint).unwrap();
            let mut parser = vt100::Parser::new(rows.max(1), cols.max(1), 0);
            parser.process(paint.as_bytes());
            let screen = parser.screen();
            fs::write(directory.join(format!("{name}.txt")), plain(screen)).unwrap();
            let cells: Vec<_> = (0..rows).map(|y| (0..cols).map(|x| {
                let c = screen.cell(y,x).unwrap();
                json!({"text":c.contents(),"fg":format!("{:?}",c.fgcolor()),"bg":format!("{:?}",c.bgcolor()),"bold":c.bold(),"dim":c.dim(),"inverse":c.inverse(),"wide_continuation":c.is_wide_continuation()})
            }).collect::<Vec<_>>()).collect();
            fs::write(directory.join(format!("{name}.json")), serde_json::to_vec(&json!({"rows":rows,"cols":cols,"cells":cells,"cursor":screen.cursor_position(),"hidden":screen.hide_cursor()})).unwrap()).unwrap();
        }
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
    screen.cell(y, x).unwrap().contents()
}
fn row(screen: &Screen, y: u16) -> String {
    (0..screen.size().1).map(|x| text(screen, y, x)).collect()
}

#[test]
fn borderless_surface_bottom_bar_edges_and_tiny_viewports() {
    let s = Server::start();
    let v = s.attach();
    s.screen(v); // negotiate the PTY before positioning output
    s.run(
        v,
        r#"exec /bin/sh -c 'printf "\033[2J\033[H\033[31;44mEDGE\033[23;80HZ\033[5;6H"; exec sleep 60'"#,
    );
    eventually(|| {
        let screen = s.painted(v, 24, 80);
        text(&screen, 22, 79) == "Z" && screen.cursor_position() == (4, 5)
    });
    let screen = s.painted(v, 24, 80);
    assert_eq!(&row(&screen, 0)[..4], "EDGE");
    assert_eq!(screen.cursor_position(), (4, 5));
    assert!(!screen.hide_cursor());
    assert_eq!(screen.cell(0, 0).unwrap().fgcolor(), Color::Idx(1));
    assert_eq!(screen.cell(0, 0).unwrap().bgcolor(), Color::Idx(4));
    assert_eq!(screen.cell(22, 79).unwrap().bgcolor(), Color::Idx(4));
    for col in 0..80 {
        assert_eq!(screen.cell(23, col).unwrap().bgcolor(), Color::Idx(8));
    }
    assert!(row(&screen, 23).starts_with(" main"));
    assert!(!screen.contents().contains('+'));
    s.capture(v, 24, 80, "single");
    for (rows, cols) in [(1, 80), (2, 1), (1, 1), (0, 0), (24, 80)] {
        s.resize(v, rows, cols);
        let screen = s.painted(v, rows, cols);
        if rows == 1 && cols > 0 {
            assert_eq!(screen.cell(0, cols - 1).unwrap().bgcolor(), Color::Idx(8));
            assert!(screen.hide_cursor());
        }
        if rows == 0 {
            assert_eq!(screen.contents(), "");
        }
    }
    s.resize(v, 1, 20);
    s.capture(v, 1, 20, "tiny");
}

#[test]
fn native_splits_junctions_focus_zoom_and_no_margin_chrome() {
    let s = Server::start();
    let v = s.attach();
    s.resize(v, 15, 61);
    s.control(v, "split_horizontal", "");
    s.control(v, "split_vertical", "");
    let screen = s.painted(v, 15, 61);
    // Native layout gives left width 30, right width 30 and one shared gap.
    assert_eq!(text(&screen, 0, 30), "│");
    assert_eq!(text(&screen, 7, 30), "├");
    assert_eq!(text(&screen, 7, 31), "─");
    assert_eq!(text(&screen, 7, 60), "─");
    assert!(screen.cell(10, 30).unwrap().bold());
    assert_eq!(screen.cell(0, 30).unwrap().fgcolor(), Color::Idx(8));
    s.capture(v, 15, 61, "nested");
    let before = s.viewer(v)["focus"].clone();
    s.mouse(v, "press", 0, 0);
    assert_ne!(s.viewer(v)["focus"], before);
    let screen = s.painted(v, 15, 61);
    assert!(screen.cell(0, 30).unwrap().bold());
    assert!(!screen.cell(7, 50).unwrap().bold());
    let selected = s.viewer(v)["focus"].clone();
    s.mouse(v, "press", 30, 3); // separator cannot pick a pane
    s.mouse(v, "press", 0, 14); // bottom bar cannot pick a pane
    assert_eq!(s.viewer(v)["focus"], selected);
    s.control(v, "zoom", "");
    let screen = s.painted(v, 15, 61);
    assert!(!screen.contents().contains('│') || row(&screen, 14).contains('│'));
    assert_eq!(text(&screen, 0, 30), " ");
    assert_eq!(text(&screen, 7, 31), " ");
    s.control(v, "zoom", "");
    s.painted(v, 15, 61);
    // Preserve arbitrary native spacing instead of painting every empty cell.
    let nodes = s.query("bevy_ui::ui_node::Node");
    for split in s.query("fux::model::Split") {
        let mut node = nodes
            .iter()
            .find(|n| n["entity"] == split["entity"])
            .unwrap()["components"]["bevy_ui::ui_node::Node"]
            .clone();
        node["column_gap"] = json!({"Px":5.0});
        node["row_gap"] = json!({"Px":5.0});
        s.rpc(
            "world.insert_components",
            json!({"entity":split["entity"],"components":{"bevy_ui::ui_node::Node":node}}),
        );
    }
    let screen = s.painted(v, 15, 61);
    for y in 0..14 {
        assert!(!row(&screen, y).contains(['│', '─', '├']));
    }
}

#[test]
fn command_column_prefix_policy_scroll_prompts_and_repaint() {
    let s = Server::start();
    let v = s.attach();
    s.resize(v, 12, 60);
    let live = s.painted(v, 12, 60);
    s.key(v, "b", true);
    let panel = s.painted(v, 12, 60);
    assert!(panel.hide_cursor());
    assert!(panel.contents().contains("Commands"));
    assert!(panel.contents().contains("split side by side"));
    assert!(panel.contents().contains("▼"));
    assert_eq!(panel.cell(10, 59).unwrap().bgcolor(), Color::Idx(8));
    assert!(row(&panel, 11).starts_with(" main"));
    assert_eq!(panel.cell(0, 0).unwrap().bgcolor(), Color::Default);
    s.key(v, "f12", false);
    assert_eq!(s.viewer(v)["prefix"], true);
    s.input(v, json!({"kind":"paste","text":"NOT-PTY-INPUT"}));
    s.key(v, "escape", false);
    let closed = s.painted(v, 12, 60);
    assert_eq!(closed.hide_cursor(), live.hide_cursor());
    assert!(!closed.contents().contains("Commands"));
    assert!(!closed.contents().contains("NOT-PTY-INPUT"));
    // Modified arrow invokes resize; unmodified arrows belong to the list.
    let focus = s.viewer(v)["focus"].clone();
    let grow = || {
        s.query("bevy_ui::ui_node::Node")
            .into_iter()
            .find(|n| n["entity"] == focus)
            .unwrap()["components"]["bevy_ui::ui_node::Node"]["flex_grow"]
            .as_f64()
            .unwrap()
    };
    let before = grow();
    s.key(v, "b", true);
    s.key(v, "down", false);
    assert_eq!(grow(), before);
    assert_eq!(s.viewer(v)["help_scroll"], 1);
    s.key(v, "right", true);
    assert!(grow() > before);
    assert_eq!(s.viewer(v)["prefix"], false);
    assert_eq!(s.viewer(v)["help_scroll"], 1);
    s.key(v, "b", true);
    s.key(v, "?", false);
    s.painted(v, 12, 60);
    s.mouse(v, "scrolldown", 59, 10);
    assert_eq!(s.viewer(v)["help_scroll"], 1);
    s.mouse(v, "scrollup", 59, 10);
    assert_eq!(s.viewer(v)["help_scroll"], 0);
    s.key(v, "down", false);
    assert_eq!(s.viewer(v)["help_scroll"], 1);
    s.key(v, "pagedown", false);
    assert!(s.viewer(v)["help_scroll"].as_u64().unwrap() > 1);
    let screen = s.painted(v, 12, 60);
    assert!(screen.contents().contains("▲"));
    let focus = s.viewer(v)["focus"].clone();
    s.mouse(v, "press", 0, 0);
    assert_eq!(s.viewer(v)["focus"], focus);
    s.input(v, json!({"kind":"paste","text":"NOT-HELP-INPUT"}));
    assert_eq!(s.viewer(v)["buffer"], "");
    for _ in 0..60 {
        s.key(v, "down", false);
    }
    assert!(s.painted(v, 12, 60).contents().contains("?  command help"));
    s.resize(v, 60, 60);
    assert!(
        s.painted(v, 60, 60)
            .contents()
            .contains("split side by side")
    );
    s.key(v, "home", false);
    assert_eq!(s.viewer(v)["help_scroll"], 0);
    s.capture(v, 60, 60, "help");
    s.resize(v, 7, 18);
    s.capture(v, 7, 18, "narrow-help");
    s.key(v, "escape", false);
    s.resize(v, 12, 60);
    s.control(v, "rename_pane", "");
    s.input(v, json!({"kind":"paste","text":"界é-long-name"}));
    s.key(v, "backspace", false);
    let screen = s.painted(v, 12, 60);
    assert!(screen.contents().contains("rename pane"));
    assert!(screen.contents().contains("界é-long-nam▏"));
    assert!(screen.hide_cursor());
    let input_row = 9;
    assert!((0..60).any(|x| screen.cell(input_row, x).unwrap().inverse()));
    s.capture(v, 12, 60, "prompt");
    s.key(v, "escape", false);
    assert!(!s.painted(v, 12, 60).contents().contains("rename pane"));
    s.control(v, "rename_pane", "");
    s.input(v, json!({"kind":"paste","text":"renamed"}));
    s.enter(v);
    assert!(row(&s.painted(v, 12, 60), 11).contains("renamed"));
}
