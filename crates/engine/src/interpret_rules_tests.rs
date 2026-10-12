use std::path::{Path, PathBuf};
use std::sync::Arc;

use filmcraft_color::ColorSpace;
use filmcraft_project::ItemId;
use serde_json::{Value, json};

use crate::Session;
use crate::autosave::Preferences;

fn tmp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("filmcraft-rules-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A 0.1 s mono 8 kHz WAV: the smallest media file the engine imports without a codec.
fn write_wav(path: &Path) {
    write_wav_samples(path, 800);
}

/// A mono 8 kHz WAV of `n` samples (n / 8000 seconds).
fn write_wav_samples(path: &Path, n: usize) {
    let data = vec![128u8; n];
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&8000u32.to_le_bytes());
    b.extend_from_slice(&8000u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&8u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(data.len() as u32).to_le_bytes());
    b.extend_from_slice(&data);
    std::fs::write(path, b).unwrap();
}

fn import(s: &mut Session, path: &Path) -> ItemId {
    let r = s.execute("file.import", json!({"paths": [path.to_string_lossy()]})).unwrap();
    ItemId(r["items"][0].as_u64().unwrap_or_else(|| panic!("{r}")))
}

fn space(s: &Session, id: ItemId) -> Option<ColorSpace> {
    s.project.item(id).unwrap().as_media().unwrap().interpret.color_space
}

fn nlog_rules() -> Value {
    json!([
        {"name": "raw", "extensions": [".NEV", "wav"], "colorSpace": "nlog-bt2020"},
        {"name": "proxy", "companion": {"extension": "nev", "dirs": ["", "../raw"]}, "colorSpace": "nlog-bt2020"},
    ])
}

#[test]
fn without_rules_an_import_is_interpreted_as_the_file_says() {
    let d = tmp_dir("none");
    write_wav(&d.join("a.wav"));
    let mut s = Session::default();
    let id = import(&mut s, &d.join("a.wav"));
    assert_eq!(space(&s, id), None);
    assert!(s.execute("import.rules", json!({})).unwrap()["rules"].as_array().unwrap().is_empty());
}

#[test]
fn a_matching_rule_sets_the_colour_space_in_the_import_undo_step() {
    let d = tmp_dir("ext");
    write_wav(&d.join("a.wav"));
    let mut s = Session::default();
    s.execute("import.setRules", json!({"rules": nlog_rules()})).unwrap();
    let before = s.history.undo.len();
    let id = import(&mut s, &d.join("a.wav"));
    assert_eq!(space(&s, id), Some(ColorSpace::NLogBt2020));
    assert_eq!(s.history.undo.len(), before + 1, "import and interpretation are one undo step");
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.item(id).is_none(), "one undo removes the clip, not just its colour space");
}

#[test]
fn a_clip_with_its_raw_clip_in_a_listed_folder_matches_and_one_without_does_not() {
    let d = tmp_dir("companion");
    std::fs::create_dir_all(d.join("proxies")).unwrap();
    std::fs::create_dir_all(d.join("raw")).unwrap();
    write_wav(&d.join("proxies/DSC_1.wav"));
    write_wav(&d.join("proxies/DSC_2.wav"));
    std::fs::write(d.join("raw/dsc_1.NEV"), b"x").unwrap();
    let mut s = Session::default();
    // only the proxy rule: the wav extension is not listed
    s.execute("import.setRules", json!({"rules": [nlog_rules()[1].clone()]})).unwrap();
    let linked = import(&mut s, &d.join("proxies/DSC_1.wav"));
    let unlinked = import(&mut s, &d.join("proxies/DSC_2.wav"));
    assert_eq!(space(&s, linked), Some(ColorSpace::NLogBt2020));
    assert_eq!(space(&s, unlinked), None);
}

#[test]
fn applying_rules_to_clips_already_imported_keeps_the_users_own_override() {
    let d = tmp_dir("apply");
    write_wav(&d.join("a.wav"));
    write_wav(&d.join("b.wav"));
    let mut s = Session::default();
    let (a, b) = (import(&mut s, &d.join("a.wav")), import(&mut s, &d.join("b.wav")));
    s.execute("clip.interpretFootage", json!({"items": [b.0], "colorSpace": "rec2100-pq"})).unwrap();
    s.execute("import.setRules", json!({"rules": nlog_rules()})).unwrap();

    let before = s.history.undo.len();
    let r = s.execute("project.applyInterpretRules", json!({})).unwrap();
    assert_eq!(r["changed"].as_array().unwrap().len(), 1, "{r}");
    assert_eq!(r["keptExistingOverride"], 1);
    assert_eq!(space(&s, a), Some(ColorSpace::NLogBt2020));
    assert_eq!(space(&s, b), ColorSpace::parse("rec2100-pq"));
    assert_eq!(s.history.undo.len(), before + 1);

    // nothing left to change: no new undo step
    let r = s.execute("project.applyInterpretRules", json!({})).unwrap();
    assert!(r["changed"].as_array().unwrap().is_empty());
    assert_eq!(s.history.undo.len(), before + 1);

    // overwrite takes the user's choice back too, and one undo restores both
    let r = s.execute("project.applyInterpretRules", json!({"overwrite": true})).unwrap();
    assert_eq!(r["changed"].as_array().unwrap().len(), 1);
    assert_eq!(space(&s, b), Some(ColorSpace::NLogBt2020));
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(space(&s, b), ColorSpace::parse("rec2100-pq"));
    assert_eq!(space(&s, a), Some(ColorSpace::NLogBt2020));
}

#[test]
fn rules_load_from_a_json_file_and_a_bad_file_changes_nothing() {
    let d = tmp_dir("file");
    let good = d.join("good.json");
    std::fs::write(&good, serde_json::to_vec(&json!({"rules": nlog_rules()})).unwrap()).unwrap();
    let mut s = Session::default();
    let r = s.execute("import.setRules", json!({"path": good.to_string_lossy()})).unwrap();
    assert_eq!(r["rules"], 2);
    assert_eq!(s.prefs.import_rules.rules[0].extensions, vec!["nev".to_string(), "wav".to_string()], "normalised");

    for (name, body) in [
        ("not-json.json", "{".to_string()),
        ("bad-space.json", json!([{"extensions": ["nev"], "colorSpace": "nope"}]).to_string()),
        ("empty-rule.json", json!([{"colorSpace": "rec709"}]).to_string()),
        ("huge.json", " ".repeat((1 << 20) + 1)),
    ] {
        let p = d.join(name);
        std::fs::write(&p, body).unwrap();
        assert!(s.execute("import.setRules", json!({"path": p.to_string_lossy()})).is_err(), "{name}");
        assert_eq!(s.prefs.import_rules.rules.len(), 2, "{name} must leave the rules as they were");
    }
    assert!(s.execute("import.setRules", json!({"path": d.join("missing.json").to_string_lossy()})).is_err());
    assert!(s.execute("import.setRules", json!({})).is_err(), "needs rules or path");
    assert!(s.execute("import.setRules", json!({"rules": [], "path": "x"})).is_err(), "not both");
    s.execute("import.setRules", json!({"rules": []})).unwrap();
    assert!(s.prefs.import_rules.rules.is_empty(), "an empty list clears the rules");
}

#[test]
fn preferences_files_from_before_import_rules_still_load() {
    let old: Preferences = serde_json::from_str(r#"{"general": {}}"#).unwrap();
    assert!(old.import_rules.rules.is_empty());
    let mut p = Preferences::default();
    p.import_rules.rules = serde_json::from_value(nlog_rules()).unwrap();
    let back: Preferences = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
    assert_eq!(back.import_rules, p.import_rules);
}

fn proxy_rules() -> Value {
    json!([{"name": "wav with a proxy", "extensions": ["wav"], "proxy": {"extension": "wav", "dirs": ["Proxy"]}, "colorSpace": "rec709"}])
}

fn proxy_of(s: &Session, id: ItemId) -> Option<String> {
    match &s.project.item(id).unwrap().as_media().unwrap().proxy {
        Some(filmcraft_project::MediaRef::File { path }) => Some(path.clone()),
        _ => None,
    }
}

/// A clip and its proxy in the job layout (the clip in a folder, the proxy of the same name in `Proxy` below it).
fn job(name: &str) -> PathBuf {
    let d = tmp_dir(name);
    std::fs::create_dir_all(d.join("Proxy")).unwrap();
    write_wav(&d.join("a.wav"));
    write_wav(&d.join("Proxy/a.wav"));
    // b: a clip whose proxy is twice as long, so the pair cannot be right
    write_wav_samples(&d.join("b.wav"), 800);
    write_wav_samples(&d.join("Proxy/b.wav"), 1600);
    d
}

#[test]
fn an_import_rule_attaches_the_proxy_it_names_in_the_import_undo_step() {
    let d = job("proxy-attach");
    let mut s = Session::default();
    s.execute("import.setRules", json!({"rules": proxy_rules()})).unwrap();
    let before = s.history.undo.len();
    let r = s.execute("file.import", json!({"paths": [d.join("a.wav").to_string_lossy()]})).unwrap();
    let id = ItemId(r["items"][0].as_u64().unwrap());
    let proxy = proxy_of(&s, id).expect("the proxy is attached");
    assert!(proxy.ends_with("/Proxy/a.wav"), "{proxy}");
    assert_eq!(r["proxies"][0]["item"], id.0, "{r}");
    assert_eq!(r["proxies"][0]["proxy"], proxy);
    assert!(r.get("warnings").is_none(), "{r}");
    assert_eq!(s.history.undo.len(), before + 1, "the import and its proxy are one undo step");
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.item(id).is_none());
}

#[test]
fn a_proxy_that_does_not_fit_is_reported_and_not_attached() {
    let d = job("proxy-mismatch");
    let mut s = Session::default();
    s.execute("import.setRules", json!({"rules": proxy_rules()})).unwrap();
    let r = s.execute("file.import", json!({"paths": [d.join("b.wav").to_string_lossy()]})).unwrap();
    let id = ItemId(r["items"][0].as_u64().unwrap());
    assert_eq!(proxy_of(&s, id), None, "the wrong proxy must not be attached");
    let w = r["warnings"][0].as_str().unwrap_or_else(|| panic!("{r}"));
    assert!(w.contains("was not attached") && w.contains("long"), "{w}");
    assert!(r.get("proxies").is_none(), "{r}");
    // the clip itself imported fine
    assert!(s.project.item(id).is_some());
}

#[test]
fn without_a_proxy_link_nothing_is_attached() {
    let d = job("proxy-none");
    let mut s = Session::default();
    s.execute("import.setRules", json!({"rules": nlog_rules()})).unwrap();
    let r = s.execute("file.import", json!({"paths": [d.join("a.wav").to_string_lossy()]})).unwrap();
    assert_eq!(proxy_of(&s, ItemId(r["items"][0].as_u64().unwrap())), None);
    assert!(r.get("proxies").is_none() && r.get("warnings").is_none());
}

#[test]
fn applying_rules_attaches_proxies_to_clips_already_in_the_project() {
    let d = job("proxy-apply");
    let mut s = Session::default();
    let (a, b) = (import(&mut s, &d.join("a.wav")), import(&mut s, &d.join("b.wav")));
    assert_eq!((proxy_of(&s, a), proxy_of(&s, b)), (None, None));
    s.execute("import.setRules", json!({"rules": proxy_rules()})).unwrap();
    let before = s.history.undo.len();
    let r = s.execute("project.applyInterpretRules", json!({})).unwrap();
    assert_eq!(r["proxiesAttached"].as_array().unwrap().len(), 1, "{r}");
    assert_eq!(r["proxiesAttached"][0]["item"], a.0);
    assert_eq!(r["warnings"].as_array().unwrap().len(), 1, "b's proxy is the wrong length: {r}");
    assert!(proxy_of(&s, a).is_some());
    assert_eq!(proxy_of(&s, b), None);
    assert_eq!(s.history.undo.len(), before + 1, "one undo step");
    // a clip that already has a proxy keeps it, and applying again changes nothing
    let r = s.execute("project.applyInterpretRules", json!({})).unwrap();
    assert!(r["proxiesAttached"].as_array().unwrap().is_empty());
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(proxy_of(&s, a), None);
}

/// The attached proxy is what the media pool hands out when proxies are on, the clip itself otherwise (and for export).
#[test]
fn the_pool_plays_the_proxy_when_proxies_are_on_and_the_clip_for_export() {
    let d = job("proxy-pool");
    let mut s = Session::default();
    s.execute("import.setRules", json!({"rules": proxy_rules()})).unwrap();
    let r = s.execute("file.import", json!({"paths": [d.join("a.wav").to_string_lossy()]})).unwrap();
    let id = ItemId(r["items"][0].as_u64().unwrap());
    let proxy_dir = d.join("Proxy").to_string_lossy().into_owned();
    let name_of = |src: filmcraft_media::SharedSource| src.info().name.clone();
    assert_eq!(name_of(s.media.full_res_source(&s.project, id, &*s.services).unwrap()), "a.wav");
    assert!(proxy_of(&s, id).unwrap().starts_with(&proxy_dir) || proxy_of(&s, id).unwrap().contains("/Proxy/"));
    // proxies on: playback reads the proxy; the full-resolution source (what export uses) is still the clip
    let mut prefs = s.prefs.clone();
    prefs.media.enable_proxies = true;
    s.set_prefs(prefs).unwrap();
    assert!(s.media.use_proxies());
    let playing = s.media.source_for(&s.project, id, &*s.services).unwrap();
    let exporting = s.media.full_res_source(&s.project, id, &*s.services).unwrap();
    assert_ne!(Arc::as_ptr(&playing) as *const u8, Arc::as_ptr(&exporting) as *const u8, "playback and export read different sources");
}
