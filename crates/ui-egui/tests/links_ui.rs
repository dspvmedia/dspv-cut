//! Headless UI test of the project links: Help menu commands, the About dialog and the Home (Import)
//! screen, and (DSPV fork) that none of them, nor the header, links the ArtCraft community.
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render the window offscreen with wgpu and write
//! `links-*.png` there; without it no GPU is needed.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use filmcraft_ui_egui::links;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    snapshots: Option<std::path::PathBuf>,
}

impl Driver {
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let snapshots = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").map(std::path::PathBuf::from);
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
        if snapshots.is_some() {
            b = b.wgpu();
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, snapshots };
        d.frames(4);
        d
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
                return v["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }

    fn snapshot(&mut self, name: &str) {
        let Some(dir) = self.snapshots.clone() else { return };
        self.frames(2);
        match self.harness.render() {
            Ok(img) => {
                std::fs::create_dir_all(&dir).unwrap();
                img.save(dir.join(format!("links-{name}.png"))).unwrap();
            }
            Err(e) => eprintln!("snapshot {name} skipped: {e}"),
        }
    }
}

#[test]
fn project_links_everywhere_and_no_artcraft_community() {
    let mut d = Driver::demo();
    // Help menu: every link is a command that reports the URL it opened.
    for (id, _, url) in links::ALL {
        let r = d.ok("engine.execute", json!({"command": id, "params": {}}));
        assert_eq!(r["url"], json!(url), "{id}");
    }
    let menu = d.ok("ui.menu.list", json!({}));
    let text = menu.to_string();
    for (id, label, _) in links::ALL {
        assert!(text.contains(id) && text.contains(label), "Help menu lists {id}");
    }
    // DSPV fork (licence audit 2026-10-22): a modified version may not present itself as part of the
    // ArtCraft community, so no menu, button or screen links the ArtCraft Discord or website.
    let no_artcraft = |what: &str, text: &str| {
        let lower = text.to_lowercase();
        for banned in ["artcraft discord", "artcraft website", "discord.gg", "getartcraft", "help.discord", "help.website", "help.appPage"] {
            assert!(!lower.contains(&banned.to_lowercase()), "{what} still has {banned:?}: {text}");
        }
    };
    no_artcraft("the menus", &text);
    // Header: no Discord button.
    assert!(d.ids("header.discord").is_empty(), "the header has no Discord button");
    no_artcraft("the header", &d.ok("ui.elements", json!({"prefix": "header."})).to_string());
    d.snapshot("header");
    // About dialog: the source code links only.
    d.ok("engine.execute", json!({"command": "app.about", "params": {}}));
    d.frames(3);
    let about = d.ids("about.");
    for id in ["about.github", "about.reportIssue"] {
        assert!(about.iter().any(|a| a == id), "{id} in {about:?}");
    }
    for id in ["about.discord", "about.website", "about.appPage"] {
        assert!(!about.iter().any(|a| a == id), "{id} is gone from {about:?}");
    }
    no_artcraft("the About dialog", &d.ok("ui.elements", json!({"prefix": "about."})).to_string());
    d.snapshot("about");
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(3);
    assert!(d.ids("about.").is_empty(), "Escape closes About");
    // Home screen (Import mode): the source code link, labelled as itself (upstream's first link was
    // relabelled "Join us on Discord").
    d.ok("engine.execute", json!({"command": "mode.import", "params": {}}));
    d.frames(3);
    assert_eq!(d.ids("import.link."), vec!["import.link.github".to_string()]);
    let home = d.ok("ui.elements", json!({"prefix": "import.link."}));
    assert_eq!(home[0]["label"], json!("FilmCraft on GitHub"), "{home}");
    no_artcraft("the Home screen", &home.to_string());
    d.snapshot("home");
}
