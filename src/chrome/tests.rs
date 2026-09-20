use super::*;
use bevy_ecs::prelude::Entity;

fn viewer(rows: u16, cols: u16) -> Viewer {
    Viewer {
        tab: None,
        workspace: Entity::PLACEHOLDER,
        focus: None,
        rows,
        cols,
        zoom: false,
        scrollback: 0,
        notice: String::new(),
        notice_error: false,
        help_scroll: 0,
        prefix: false,
        prompt: Some("help".into()),
        buffer: String::new(),
    }
}

#[test]
fn truncation_is_cell_sized_sanitized_and_keeps_the_requested_end() {
    for text in ["界é/path/to/long-name", "abc", "\x1b[31m\r\nhi", "🦀界", ""] {
        for cols in 0..32 {
            for tail in [false, true] {
                let shown = fit(text, cols, tail);
                assert!(width(&shown) <= cols, "{shown:?} in {cols}");
                assert!(!shown.chars().any(char::is_control));
            }
        }
    }
    assert_eq!(fit("/long/name", 5, true), "…name");
    assert_eq!(fit("notice sentence", 7, false), "notice…");
    assert_eq!(fit("界界", 3, false), "界…");
}

#[test]
fn every_binding_is_reachable_at_every_short_height() {
    let settings = Settings::default();
    for rows in 2..40 {
        let mut v = viewer(rows, 80);
        let mut seen = std::collections::BTreeSet::new();
        for offset in 0..=help_limit(&settings, rows) {
            v.help_scroll = offset;
            let mut out = String::new();
            let bounds = panel(&mut out, &v, &settings).unwrap();
            assert_eq!(bounds.y + bounds.height, rows - 1);
            assert_eq!(bounds.x + bounds.width, 80);
            let mut parser = vt100::Parser::new(rows, 80, 0);
            parser.process(out.as_bytes());
            let contents = parser.screen().contents();
            for binding in &settings.bindings {
                let label = crate::actions::metadata(&binding.action)
                    .map_or_else(|| binding.action.replace('_', " "), |a| a.label.into());
                if contents.contains(&label) {
                    seen.insert(binding.action.clone());
                }
            }
            assert_eq!(
                parser.screen().cell(rows - 1, 79).unwrap().bgcolor(),
                vt100::Color::Default
            );
        }
        assert_eq!(seen.len(), settings.bindings.len(), "{rows} rows: {seen:?}");
    }
}

#[test]
fn panel_is_content_sized_above_a_full_width_bar_and_resets_styles() {
    let settings = Settings {
        bindings: vec![crate::assets::Binding {
            key: "k".into(),
            action: "known_action".into(),
        }],
        ..Default::default()
    };
    let v = viewer(12, 40);
    let mut out = "\x1b[31;44;7m".to_owned();
    bar(&mut out, &v, "workspace", "7: pane");
    let bounds = panel(&mut out, &v, &settings).unwrap();
    assert_eq!(bounds.height, 3);
    assert_eq!(bounds.width, width("k  known action") + 2);
    let mut parser = vt100::Parser::new(12, 40, 0);
    parser.process(out.as_bytes());
    let screen = parser.screen();
    for x in 0..40 {
        assert_eq!(screen.cell(11, x).unwrap().bgcolor(), vt100::Color::Idx(8));
        assert!(!screen.cell(11, x).unwrap().inverse());
    }
    assert!(screen.cell(bounds.y, bounds.x + 1).unwrap().bold());
    assert!(screen.cell(bounds.y + 1, bounds.x + 1).unwrap().bold());
    assert!(!screen.cell(bounds.y + 2, bounds.x + 1).unwrap().bold());
    assert_eq!(
        screen.cell(bounds.y, bounds.x - 1).unwrap().bgcolor(),
        vt100::Color::Default
    );
    assert_eq!(screen.bgcolor(), vt100::Color::Default);
    assert!(!screen.inverse());
}

#[test]
fn overflowing_unicode_tab_bar_keeps_active_cells_and_pick_bounds_inside_viewport() {
    let tabs: Vec<_> = (0..12)
        .map(|i| (Entity::from_bits(i + 1), format!("界é-tab-{i}")))
        .collect();
    for cols in 0..100 {
        for rows in 0..3 {
            let mut v = viewer(rows, cols);
            v.tab = Some(tabs[9].0);
            let mut out = String::new();
            let hits = tab_bar(&mut out, &v, "workspace", &tabs, "process");
            if rows == 0 || cols == 0 {
                assert!(out.is_empty());
                assert!(hits.is_empty());
                continue;
            }
            assert!(
                hits.iter()
                    .any(|(id, bounds)| Some(*id) == v.tab && bounds.width > 0),
                "{rows}x{cols}"
            );
            for (_, bounds) in &hits {
                assert!(bounds.x + bounds.width <= cols);
                assert_eq!(bounds.y + bounds.height, rows);
            }
            let mut parser = vt100::Parser::new(rows.max(2), cols.max(2), 0);
            parser.process(out.as_bytes());
            let active = hits.iter().find(|(id, _)| Some(*id) == v.tab).unwrap().1;
            assert!(parser.screen().cell(rows - 1, active.x).unwrap().inverse());
            for x in 0..cols {
                // vt100 stores a wide glyph's attributes on its leading cell.
                if parser
                    .screen()
                    .cell(rows - 1, x)
                    .unwrap()
                    .is_wide_continuation()
                {
                    continue;
                }
                assert_eq!(
                    parser.screen().cell(rows - 1, x).unwrap().bgcolor(),
                    vt100::Color::Idx(8),
                    "{rows}x{cols} cell {x}: {out:?}"
                );
            }
        }
    }
}
