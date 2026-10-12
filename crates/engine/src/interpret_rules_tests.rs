use std::path::{Path, PathBuf};

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
    let data = vec![128u8; 800];
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
