use super::*;
use crate::testing::*;

/// The policy of a server as `ServerPlugin` builds it.
fn served() -> Vec<(String, ReflectPolicy)> {
    let mut app = App::new();
    app.insert_resource(Wake(std::thread::current()));
    app.add_plugins((
        bevy_app::TaskPoolPlugin::default(),
        bevy_asset::AssetPlugin::default(),
        crate::server::ServerPlugin,
    ));
    table(app.world())
}

/// The README's policy table matches the code, row for row, so what clients
/// read is what the guard enforces.
#[test]
fn the_readme_table_is_the_policy() -> Outcome {
    let readme = include_str!("../../README.md");
    let start = readme.find("<!-- policy-table -->").need()?;
    let end = readme.find("<!-- /policy-table -->").need()?;
    let yes = |b: bool| if b { "✓" } else { "" };
    let documented: Vec<String> = readme
        .get(start..end)
        .need()?
        .lines()
        .filter(|line| line.starts_with("| `"))
        .map(str::to_owned)
        .collect();
    let expected: Vec<String> = served()
        .into_iter()
        .map(|(path, p)| {
            format!(
                "| `{path}` | {} | {} | {} | {} | {} | {} |",
                yes(p.access.write),
                yes(p.access.spawn),
                yes(p.access.remove && !p.required),
                yes(p.access.trigger),
                yes(p.required),
                p.note
            )
        })
        .collect();
    assert_eq!(documented, expected);
    Ok(())
}

/// Opening a type is deliberate: every opened type is fux's own or one the
/// layout needs, and nothing else Bevy registers is writable.
#[test]
fn only_deliberate_types_are_open() -> Outcome {
    let opened: Vec<String> = served().into_iter().map(|(path, _)| path).collect();
    for path in &opened {
        assert!(
            path.starts_with("fux::")
                || [
                    "bevy_ecs::hierarchy::ChildOf",
                    "bevy_ecs::name::Name",
                    "bevy_ui::ui_node::Node",
                    "bevy_camera::visibility::Visibility",
                ]
                .contains(&path.as_str()),
            "{path} is open"
        );
    }
    assert!(opened.len() >= 19, "{opened:?}");
    Ok(())
}
