//! Which writes the hub page is told about. Pure, so it runs natively although the rest of the
//! crate is wasm32-only.

#[path = "../src/host_save.rs"]
mod host_save;

use host_save::{WriteRoute, is_project_write, route_write};

#[test]
fn saved_projects_are_project_writes() {
    assert!(is_project_write("/projects/Dune reel.fcproj"));
    assert!(is_project_write("/projects/Dune.FCPROJ"));
}

#[test]
fn exports_captions_and_other_folders_are_not() {
    for path in ["/exports/a.mp4", "/exports/Dune.fcproj", "/projects/", "/projects/.fcproj.txt", "/projects/clip.srt", "/opfs/recovery.fcproj", "projects/a.fcproj", "/recovered/x.fcproj"] {
        assert!(!is_project_write(path), "{path}");
    }
}

#[test]
fn only_a_listening_host_gets_projects_and_everything_else_downloads() {
    assert_eq!(route_write("/projects/a.fcproj", true), WriteRoute::Host);
    assert_eq!(route_write("/projects/a.fcproj", false), WriteRoute::Download);
    assert_eq!(route_write("/exports/a.mp4", true), WriteRoute::Download);
}
