use super::*;
use crate::testing::*;
use bevy_ecs::prelude::Entity;

fn viewer(rows: u16, cols: u16) -> Viewer {
    Viewer {
        rows,
        cols,
        zoom: false,
        scrollback: 0,
        notice: None,
    }
}

#[test]
fn truncation_is_cell_sized_sanitized_and_keeps_the_requested_end() -> crate::testing::Outcome {
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
    Ok(())
}

#[test]
fn every_binding_is_reachable_at_every_short_height() -> crate::testing::Outcome {
    let settings = Settings::default();
    for rows in 2..40 {
        let v = viewer(rows, 80);
        let mut seen = std::collections::BTreeSet::new();
        for offset in 0..=help_limit(&settings, rows) {
            let mut out = String::new();
            let bounds = panel(&mut out, &v, &settings, offset).need()?;
            assert_eq!(bounds.y + bounds.height, rows - 1);
            assert_eq!(bounds.x + bounds.width, 80);
            let mut parser = vt100::Parser::new(rows, 80, 0);
            parser.process(out.as_bytes());
            let contents = parser.screen().contents();
            for binding in &settings.bindings {
                let label = match &binding.action {
                    BindingAction::Known(action) => action.label().to_owned(),
                    BindingAction::Custom(name) => name.replace('_', " "),
                };
                if contents.contains(&label) {
                    seen.insert(binding.action.to_string());
                }
            }
            assert_eq!(
                parser.screen().cell(rows - 1, 79).need()?.bgcolor(),
                vt100::Color::Default
            );
        }
        assert_eq!(seen.len(), settings.bindings.len(), "{rows} rows: {seen:?}");
    }
    Ok(())
}

#[test]
fn tiny_unicode_command_selection_is_visible_even_when_disabled() -> crate::testing::Outcome {
    let settings = Settings {
        bindings: vec![
            crate::assets::Binding {
                key: "界".into(),
                action: BindingAction::Custom("custom_界é".into()),
            },
            crate::assets::Binding {
                key: "x".into(),
                action: BindingAction::Known(crate::actions::Action::Close),
            },
        ],
        ..Default::default()
    };
    for rows in 0..=4 {
        for cols in 0..=2 {
            for selected in 0..2 {
                let v = viewer(rows, cols);
                let mut out = String::new();
                let bounds = panel_context(&mut out, &v, &settings, selected, |_| true);
                if rows < 2 || cols == 0 {
                    assert!(bounds.is_none());
                    assert!(out.is_empty());
                    continue;
                }
                let mut parser = vt100::Parser::new(rows.max(2), cols.max(2), 0);
                parser.process(out.as_bytes());
                assert!((0..rows - 1).any(|y| (0..cols).any(|x| {
                    parser
                        .screen()
                        .cell(y, x)
                        .is_some_and(|c| c.inverse() && c.dim() && !c.bold())
                })));
            }
        }
    }
    Ok(())
}

#[test]
fn panel_is_content_sized_above_a_full_width_bar_and_resets_styles() -> crate::testing::Outcome {
    let settings = Settings {
        bindings: vec![crate::assets::Binding {
            key: "k".into(),
            action: BindingAction::Custom("known_action".into()),
        }],
        ..Default::default()
    };
    let v = viewer(12, 40);
    let mut out = "\x1b[31;44;7m".to_owned();
    bar(&mut out, &v, "workspace", "7: pane");
    let bounds = panel(&mut out, &v, &settings, 0).need()?;
    assert_eq!(bounds.height, 3);
    assert_eq!(bounds.width, width("k  known action") + 2);
    let mut parser = vt100::Parser::new(12, 40, 0);
    parser.process(out.as_bytes());
    let screen = parser.screen();
    for x in 0..40 {
        assert_eq!(screen.cell(11, x).need()?.bgcolor(), vt100::Color::Idx(8));
        assert!(!screen.cell(11, x).need()?.inverse());
    }
    assert!(screen.cell(bounds.y, bounds.x + 1).need()?.bold());
    assert!(screen.cell(bounds.y + 1, bounds.x + 1).need()?.bold());
    assert!(!screen.cell(bounds.y + 2, bounds.x + 1).need()?.bold());
    assert_eq!(
        screen.cell(bounds.y, bounds.x - 1).need()?.bgcolor(),
        vt100::Color::Default
    );
    assert_eq!(screen.bgcolor(), vt100::Color::Default);
    assert!(!screen.inverse());
    Ok(())
}

#[test]
fn overflowing_unicode_tab_bar_keeps_active_cells_and_pick_bounds_inside_viewport()
-> crate::testing::Outcome {
    let tabs: Vec<_> = (0..12)
        .map(|i| (Entity::from_bits(i + 1), format!("界é-tab-{i}")))
        .collect();
    for cols in 0..100 {
        for rows in 0..3 {
            let v = viewer(rows, cols);
            let tab = Some(tabs.get(9).need()?.0);
            let mut out = String::new();
            let hits = tab_bar(
                &mut out,
                &v,
                (Entity::PLACEHOLDER, "workspace"),
                (tab, &tabs),
                "process",
            );
            if rows == 0 || cols == 0 {
                assert!(out.is_empty());
                assert!(hits.is_empty());
                continue;
            }
            assert!(
                hits.iter()
                    .any(|(id, bounds)| Some(*id) == tab && bounds.width > 0),
                "{rows}x{cols}"
            );
            for (_, bounds) in &hits {
                assert!(bounds.x + bounds.width <= cols);
                assert_eq!(bounds.y + bounds.height, rows);
            }
            let mut parser = vt100::Parser::new(rows.max(2), cols.max(2), 0);
            parser.process(out.as_bytes());
            let active = hits.iter().find(|(id, _)| Some(*id) == tab).need()?.1;
            assert!(parser.screen().cell(rows - 1, active.x).need()?.inverse());
            for x in 0..cols {
                // vt100 stores a wide glyph's attributes on its leading cell.
                if parser
                    .screen()
                    .cell(rows - 1, x)
                    .need()?
                    .is_wide_continuation()
                {
                    continue;
                }
                assert_eq!(
                    parser.screen().cell(rows - 1, x).need()?.bgcolor(),
                    vt100::Color::Idx(8),
                    "{rows}x{cols} cell {x}: {out:?}"
                );
            }
        }
    }
    Ok(())
}
