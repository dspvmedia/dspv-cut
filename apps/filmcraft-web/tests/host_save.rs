//! Which writes the hub page is told about. Pure, so it runs natively although the rest of the
//! crate is wasm32-only.

#[path = "../src/host_save.rs"]
mod host_save;

use host_save::{WriteRoute, is_project_write, parse_host_commands, route_write};
use serde_json::json;

#[test]
fn saved_projects_are_project_writes() {
    assert!(is_project_write("/projects/Dune reel.fcproj"));
    assert!(is_project_write("/projects/Dune.FCPROJ"));
}

#[test]
fn exports_captions_and_other_folders_are_not() {
    for path in [
        "/exports/a.mp4",
        "/exports/Dune.fcproj",
        "/projects/",
        "/projects/.fcproj.txt",
        "/projects/clip.srt",
        "/opfs/recovery.fcproj",
        "projects/a.fcproj",
        "/recovered/x.fcproj",
    ] {
        assert!(!is_project_write(path), "{path}");
    }
}

#[test]
fn only_a_listening_host_gets_projects_and_everything_else_downloads() {
    assert_eq!(route_write("/projects/a.fcproj", true), WriteRoute::Host);
    assert_eq!(route_write("/projects/a.fcproj", false), WriteRoute::Download);
    assert_eq!(route_write("/exports/a.mp4", true), WriteRoute::Download);
}

#[test]
fn a_page_can_add_commands_to_the_editors_own_menus() {
    let list = json!([{"id": "hub.sendToReview", "label": "Send to Review", "menu": ["File"]}, {"id": "hub.help", "label": "About the hub", "menu": ["Help", "More"]}]);
    assert_eq!(
        parse_host_commands(&list),
        vec![
            ("hub.sendToReview".into(), "Send to Review".into(), vec!["File".into()]),
            ("hub.help".into(), "About the hub".into(), vec!["Help".into(), "More".into()])
        ]
    );
}

#[test]
fn malformed_entries_are_dropped_and_the_list_is_capped() {
    let long = "x".repeat(65);
    let bad = json!([
        {"label": "no id", "menu": ["File"]}, {"id": "a", "menu": ["File"]}, {"id": "b", "label": "  ", "menu": ["File"]},
        {"id": "c", "label": "no menu"}, {"id": "d", "label": "unknown menu", "menu": ["Nope"]}, {"id": "e", "label": "empty menu", "menu": []},
        {"id": long, "label": "long id", "menu": ["File"]}, {"id": "f", "label": 5, "menu": ["File"]}, "text", 7, null,
        {"id": "ok", "label": "Fine", "menu": ["File"]}, {"id": "ok", "label": "Same id again", "menu": ["File"]}
    ]);
    assert_eq!(parse_host_commands(&bad), vec![("ok".into(), "Fine".into(), vec!["File".into()])]);
    assert!(parse_host_commands(&json!("not a list")).is_empty() && parse_host_commands(&json!(null)).is_empty());
    let many: Vec<_> = (0..20).map(|i| json!({"id": format!("c{i}"), "label": "L", "menu": ["File"]})).collect();
    assert_eq!(parse_host_commands(&json!(many)).len(), 8);
}
