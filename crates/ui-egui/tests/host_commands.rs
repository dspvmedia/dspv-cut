//! Commands the host adds to the menus (`HostHooks::host_commands`): they appear in the menu tree,
//! run through the host's hook with the app, and the hook is back in place afterwards.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use filmcraft_engine::Session;
use filmcraft_ui_egui::{FilmcraftApp, HostCommand, menus};
use serde_json::{Value, json};

fn command() -> HostCommand {
    HostCommand { id: "host.publish".into(), label: "Publish".into(), menu: vec!["File".into()] }
}

#[test]
fn a_host_command_is_in_the_menu_tree_and_a_plain_editor_has_none() {
    let mut app = FilmcraftApp::new(Session::default());
    assert!(!menus::menu_items(&app).iter().any(|i| i.id == "host.publish"));
    app.hooks.host_commands = vec![command()];
    let items = menus::menu_items(&app);
    let item = items.iter().find(|i| i.id == "host.publish").expect("the host command is in the menu");
    assert_eq!((item.label.as_str(), item.path.as_slice(), item.enabled), ("Publish", ["File".to_string()].as_slice(), true));
    // the editor's own menus are untouched
    assert!(items.iter().any(|i| i.id == "file.save"));
}

#[test]
fn running_it_calls_the_hook_with_the_app_and_keeps_the_hook() {
    let mut app = FilmcraftApp::new(Session::default());
    app.hooks.host_commands = vec![command()];
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    app.hooks.run_host_command = Some(Box::new(move |app, id| {
        seen.fetch_add(1, Ordering::SeqCst);
        app.ui.status = format!("ran {id}");
        Ok(json!({"ran": id}))
    }));
    let ctx = egui::Context::default();
    assert_eq!(menus::invoke(&mut app, &ctx, "host.publish", json!({})), Ok(json!({"ran": "host.publish"})));
    assert_eq!(app.ui.status, "ran host.publish");
    // the hook was put back: it runs again
    assert!(menus::invoke(&mut app, &ctx, "host.publish", json!({})).is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn a_refusal_from_the_hook_goes_to_the_status_line_and_a_missing_hook_is_an_error() {
    let mut app = FilmcraftApp::new(Session::default());
    app.hooks.host_commands = vec![command()];
    let ctx = egui::Context::default();
    assert!(menus::invoke(&mut app, &ctx, "host.publish", json!({})).is_err(), "no hook: the command says it is not available");
    app.hooks.run_host_command = Some(Box::new(|_, _| Err("save the project first".to_string())));
    assert_eq!(menus::invoke(&mut app, &ctx, "host.publish", json!({})), Err("save the project first".to_string()));
    assert_eq!(app.ui.status, "save the project first");
}

#[test]
fn an_id_that_is_not_a_host_command_never_reaches_the_hook() {
    let mut app = FilmcraftApp::new(Session::default());
    app.hooks.host_commands = vec![command()];
    let touched = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&touched);
    app.hooks.run_host_command = Some(Box::new(move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(Value::Null)
    }));
    let ctx = egui::Context::default();
    let _ = menus::invoke(&mut app, &ctx, "edit.undo", json!({}));
    assert_eq!(touched.load(Ordering::SeqCst), 0);
}
