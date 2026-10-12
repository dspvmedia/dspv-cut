//! Import rules: the colour space a file is interpreted with, decided by the file's name and its
//! neighbours instead of by what its metadata claims.
//!
//! Some cameras write proxies that carry no usable colour tag: a log recording is stored as an
//! ordinary Rec.709-tagged H.264 file, so the engine reads it as Rec.709 and the picture is wrong
//! until someone opens Interpret Footage on every clip. A rule fixes that once, at import.
//!
//! A rule matches a file when its extension is listed (`extensions`), or when a *companion* file
//! with the same name and the companion extension exists in the same folder or in one of the
//! companion's `dirs` (the proxy of a raw clip is found by the raw clip next to it, or in a
//! sibling folder). The first matching rule wins and sets the clip's colour space exactly as
//! `clip.interpretFootage` would, in the same undo step as the import. A clip the user already
//! interpreted is never overwritten by `project.applyInterpretRules` unless asked.
//!
//! A rule can also name the clip's proxy (`proxy`: a same-named file with some extension, in the clip's folder or in listed
//! folders). On import it is found, checked against the clip (frame rate, length, start timecode, aspect ratio) and attached with
//! the import; with "Enable proxies" on the editor then plays the proxy, and export always reads the full-resolution clip.
//!
//! Commands: `import.rules` (read), `import.setRules` (replace, from `rules` or a JSON `path`),
//! `project.applyInterpretRules` (apply to clips already in the project, one undo step).

use filmcraft_color::ColorSpace;
use filmcraft_project::{ItemId, ItemKind, MediaRef};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad};
use crate::{EngineError, Result, Services, Session};

/// Most rules a project keeps; every import walks them.
pub const MAX_RULES: usize = 64;
/// Most companion folders one rule may list.
const MAX_COMPANION_DIRS: usize = 16;
/// Most directory entries looked at per companion lookup (a folder of a million files must not
/// stall an import).
const MAX_LISTED: usize = 200_000;
/// Longest accepted rule file.
const MAX_RULE_FILE: usize = 1 << 20;

/// A file that has the same name as the clip, with this extension, in the clip's folder or in
/// one of `dirs` (relative to the clip's folder, or absolute; empty string = the clip's folder).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Companion {
    pub extension: String,
    pub dirs: Vec<String>,
}

/// Where a clip's proxy is: a file with the same name and this extension, in the clip's own folder (`""`) or in one of `dirs`
/// (relative to the clip's folder, or absolute). Found at import and attached as the clip's proxy after the pair has been
/// checked (same frame rate, length, start timecode and aspect ratio): the editor then plays the proxy and exports the clip.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProxyLink {
    pub extension: String,
    pub dirs: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct InterpretRule {
    pub name: String,
    /// Match by the clip's own extension (lower case, no dot).
    pub extensions: Vec<String>,
    /// Match by a companion file.
    pub companion: Option<Companion>,
    /// Attach the proxy found here to a clip this rule matches.
    pub proxy: Option<ProxyLink>,
    /// A colour space id (`color.spaces`), e.g. `nlog-bt2020`.
    pub color_space: String,
}

/// Preferences ▸ import rules. Outside the Settings dialog; edited with `import.setRules`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ImportRulesPrefs {
    pub rules: Vec<InterpretRule>,
}

/// File name and folder of a path written with either separator (hosts differ, and so do
/// project files carried between them).
fn split_path(path: &str) -> (&str, &str) {
    match path.rfind(['/', '\\']) {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => ("", path),
    }
}

fn split_name(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i + 1..]),
        _ => (name, ""),
    }
}

fn is_absolute(dir: &str) -> bool {
    dir.starts_with(['/', '\\']) || dir.as_bytes().get(1) == Some(&b':')
}

/// The folder a companion `dir` entry means for a clip in `media_dir`.
fn resolve_dir(media_dir: &str, dir: &str) -> String {
    if dir.is_empty() {
        media_dir.to_string()
    } else if is_absolute(dir) || media_dir.is_empty() {
        dir.to_string()
    } else {
        format!("{media_dir}/{dir}")
    }
}

/// The file called `<stem>.<ext>` (any case) in `names`, if any. Hidden files and macOS AppleDouble copies (`._name`) do not count.
fn find_named<'a>(names: &'a [String], stem: &str, ext: &str) -> Option<&'a String> {
    names.iter().take(MAX_LISTED).find(|n| {
        if n.starts_with('.') {
            return false;
        }
        let (s, e) = split_name(n);
        s.eq_ignore_ascii_case(stem) && e.eq_ignore_ascii_case(ext)
    })
}

/// Whether `rule` matches a clip called `<stem>.<ext>` in `media_dir`: by its own extension, or by a companion file.
fn rule_matches(rule: &InterpretRule, ext: &str, stem: &str, media_dir: &str, list: &dyn Fn(&str) -> Option<Vec<String>>) -> bool {
    if !ext.is_empty() && rule.extensions.iter().any(|e| e.trim_start_matches('.').eq_ignore_ascii_case(ext)) {
        return true;
    }
    let Some(c) = &rule.companion else { return false };
    if c.extension.is_empty() {
        return false;
    }
    let companion_ext = c.extension.trim_start_matches('.');
    // a clip is never its own companion (rule extension == companion extension)
    let own = ext.eq_ignore_ascii_case(companion_ext);
    let default_dirs = [String::new()];
    let dirs: &[String] = if c.dirs.is_empty() { &default_dirs } else { &c.dirs };
    for d in dirs.iter().take(MAX_COMPANION_DIRS) {
        let dir = resolve_dir(media_dir, d);
        if let Some(names) = list(&dir)
            && find_named(&names, stem, companion_ext).is_some()
            && !(own && dir == media_dir)
        {
            return true;
        }
    }
    false
}

/// The colour space the first matching rule gives the file at `path`. `list` returns the file
/// names in a folder (`None`: cannot be listed, so no companion is found there). A rule whose
/// colour space id is unknown is skipped, never an error: rules come from a file.
pub fn color_space_for(rules: &[InterpretRule], path: &str, list: &dyn Fn(&str) -> Option<Vec<String>>) -> Option<ColorSpace> {
    let (media_dir, file) = split_path(path);
    let (stem, ext) = split_name(file);
    if stem.is_empty() {
        return None;
    }
    rules.iter().take(MAX_RULES).find_map(|rule| {
        let cs = ColorSpace::parse(&rule.color_space)?;
        rule_matches(rule, ext, stem, media_dir, list).then_some(cs)
    })
}

/// `a/b/../c` as `a/c`, for a path written with `/` only (anything with another separator is left alone).
fn tidy_path(p: &str) -> String {
    if p.contains('\\') {
        return p.to_string();
    }
    let mut out: Vec<&str> = Vec::new();
    for part in p.split('/') {
        match part {
            "." => {}
            ".." if out.last().is_some_and(|l| !l.is_empty() && *l != "..") => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out.join("/")
}

/// The proxy file of the clip at `path` from the first matching rule that has a [`ProxyLink`] and finds its file. The result is a
/// path; whether the proxy fits the clip is for the caller to check.
pub fn proxy_for(rules: &[InterpretRule], path: &str, list: &dyn Fn(&str) -> Option<Vec<String>>) -> Option<String> {
    let (media_dir, file) = split_path(path);
    let (stem, ext) = split_name(file);
    if stem.is_empty() {
        return None;
    }
    for rule in rules.iter().take(MAX_RULES) {
        let Some(link) = &rule.proxy else { continue };
        let link_ext = link.extension.trim_start_matches('.');
        if link_ext.is_empty() || !rule_matches(rule, ext, stem, media_dir, list) {
            continue;
        }
        let default_dirs = [String::new()];
        let dirs: &[String] = if link.dirs.is_empty() { &default_dirs } else { &link.dirs };
        for d in dirs.iter().take(MAX_COMPANION_DIRS) {
            let dir = resolve_dir(media_dir, d);
            let Some(names) = list(&dir) else { continue };
            // never the clip itself (a proxy with the clip's own extension in the clip's own folder)
            if let Some(name) = find_named(&names, stem, link_ext).filter(|n| !(tidy_path(&dir) == tidy_path(media_dir) && n.eq_ignore_ascii_case(file))) {
                return Some(tidy_path(&if dir.is_empty() { name.clone() } else { format!("{dir}/{name}") }));
            }
        }
    }
    None
}

/// [`color_space_for`] with the host's directory listing.
pub fn resolve(rules: &[InterpretRule], path: &str, services: &dyn Services) -> Option<ColorSpace> {
    if rules.is_empty() {
        return None;
    }
    color_space_for(rules, path, &|dir| services.list_dir(dir.trim_end_matches(['/', '\\'])).and_then(|r| r.ok()))
}

/// [`proxy_for`] with the host's directory listing.
pub fn find_proxy(rules: &[InterpretRule], path: &str, services: &dyn Services) -> Option<String> {
    if rules.is_empty() {
        return None;
    }
    proxy_for(rules, path, &|dir| services.list_dir(dir.trim_end_matches(['/', '\\'])).and_then(|r| r.ok()))
}

/// Check rules from a user or a file and put them in canonical form (lower-case extensions
/// without dots).
pub fn validate(mut rules: Vec<InterpretRule>) -> std::result::Result<Vec<InterpretRule>, String> {
    if rules.len() > MAX_RULES {
        return Err(format!("at most {MAX_RULES} rules"));
    }
    for (i, r) in rules.iter_mut().enumerate() {
        let at = |m: &str| format!("rule {} ({}): {m}", i + 1, r.name);
        if r.name.chars().count() > 80 {
            return Err(at("name is longer than 80 characters"));
        }
        if ColorSpace::parse(&r.color_space).is_none() {
            return Err(at(&format!("unknown colour space `{}`", r.color_space)));
        }
        r.extensions = r.extensions.iter().map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase()).filter(|e| !e.is_empty()).collect();
        if let Some(c) = r.companion.as_mut() {
            c.extension = c.extension.trim().trim_start_matches('.').to_ascii_lowercase();
            if c.extension.is_empty() {
                return Err(at("a companion needs an extension"));
            }
            if c.dirs.len() > MAX_COMPANION_DIRS {
                return Err(at(&format!("at most {MAX_COMPANION_DIRS} companion folders")));
            }
        }
        if let Some(l) = r.proxy.as_mut() {
            l.extension = l.extension.trim().trim_start_matches('.').to_ascii_lowercase();
            if l.extension.is_empty() {
                return Err(at("a proxy needs an extension"));
            }
            if l.dirs.len() > MAX_COMPANION_DIRS {
                return Err(at(&format!("at most {MAX_COMPANION_DIRS} proxy folders")));
            }
        }
        if r.extensions.is_empty() && r.companion.is_none() {
            return Err(at("needs `extensions` or a `companion`"));
        }
    }
    Ok(rules)
}

fn spec(id: &'static str, label: &'static str, params: &'static str, run: fn(&mut Session, &Value) -> Result<Value>) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal: false }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        spec("import.rules", "List Import Rules", "{}", |s, _| Ok(json!({"rules": s.prefs.import_rules.rules}))),
        spec("import.setRules", "Set Import Rules", r#"{"rules":[rule]?,"path":str?}"#, set_rules),
        CommandSpec {
            id: "project.applyInterpretRules",
            label: "Apply Import Rules",
            menu: &[],
            shortcut: None,
            params: r#"{"items":[id]?,"overwrite":bool=false}"#,
            enabled: always,
            run: apply_rules,
            journal: true,
        },
    ]
}

/// `import.setRules`: replace the rules with `rules`, or with the JSON file at `path` (either a
/// list of rules or `{"rules": [...]}`). Nothing is kept when any rule is invalid.
fn set_rules(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "import.setRules";
    let raw: Value = match (p.get("rules"), p.get("path").and_then(Value::as_str)) {
        (Some(r), None) => r.clone(),
        (None, Some(path)) => {
            // size first: a huge (or endless) file must be refused before any of it is read into memory
            let size = s.services.file_size(path).map_err(|e| bad(CMD, format!("cannot read {path}: {e}")))?;
            if size > MAX_RULE_FILE as u64 {
                return Err(bad(CMD, format!("{path} is larger than {MAX_RULE_FILE} bytes")));
            }
            let bytes = s.services.read_file(path).map_err(|e| bad(CMD, format!("cannot read {path}: {e}")))?;
            if bytes.len() > MAX_RULE_FILE {
                return Err(bad(CMD, format!("{path} is larger than {MAX_RULE_FILE} bytes")));
            }
            let v: Value = serde_json::from_slice(&bytes).map_err(|e| bad(CMD, format!("{path} is not JSON: {e}")))?;
            v.get("rules").cloned().unwrap_or(v)
        }
        _ => return Err(bad(CMD, "pass `rules` (a list) or `path` (a JSON file), not both")),
    };
    let rules: Vec<InterpretRule> = serde_json::from_value(raw).map_err(|e| bad(CMD, format!("rules: {e}")))?;
    let rules = validate(rules).map_err(|m| bad(CMD, m))?;
    let mut prefs = s.prefs.clone();
    prefs.import_rules = ImportRulesPrefs { rules };
    if prefs != s.prefs {
        s.set_prefs(prefs).map_err(|e| EngineError::Other(format!("saving preferences: {e}")))?;
    }
    Ok(json!({"rules": s.prefs.import_rules.rules.len()}))
}

/// `project.applyInterpretRules`: give the clips already in the project the colour space their import rule says, and attach the
/// proxy it names to a clip that has none (checked the same way as at import; a pair that does not fit is reported, not
/// attached). A clip with its own colour space override is left alone unless `overwrite`; a clip that already has a proxy keeps it.
fn apply_rules(s: &mut Session, p: &Value) -> Result<Value> {
    let overwrite = p.get("overwrite").and_then(Value::as_bool).unwrap_or(false);
    let wanted: Option<Vec<ItemId>> = p.get("items").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).map(ItemId).collect());
    let rules = s.prefs.import_rules.rules.clone();
    let mut changes: Vec<(ItemId, ColorSpace)> = Vec::new();
    let mut proxies: Vec<(ItemId, String)> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let (mut matched, mut kept) = (0usize, 0usize);
    for (id, item) in s.project.items.iter() {
        if wanted.as_ref().is_some_and(|w| !w.contains(id)) {
            continue;
        }
        let ItemKind::Media(m) = &item.kind else { continue };
        let MediaRef::File { path } = &m.media else { continue };
        if m.proxy.is_none()
            && let Some(found) = find_proxy(&rules, path, &*s.services)
        {
            match crate::proxies::check_found_proxy(s, &m.info, &item.name, &found) {
                Ok(()) => proxies.push((*id, found)),
                Err(why) => warnings.push(format!("{}: the proxy {found} was not attached: {why}", item.name)),
            }
        }
        let Some(cs) = resolve(&rules, path, &*s.services) else { continue };
        matched += 1;
        if m.interpret.color_space == Some(cs) {
            continue;
        }
        if m.interpret.color_space.is_some() && !overwrite {
            kept += 1;
            continue;
        }
        changes.push((*id, cs));
    }
    if !changes.is_empty() || !proxies.is_empty() {
        s.edit("Apply Import Rules", |pr, _| {
            for (id, cs) in &changes {
                if let Some(ItemKind::Media(m)) = pr.item_mut(*id).map(|it| &mut it.kind) {
                    m.interpret.color_space = Some(*cs);
                }
            }
            for (id, path) in &proxies {
                if let Some(ItemKind::Media(m)) = pr.item_mut(*id).map(|it| &mut it.kind) {
                    m.proxy = Some(MediaRef::File { path: path.clone() });
                }
            }
            Ok(())
        })?;
    }
    for w in &warnings {
        s.error_toast("import rules", w.clone());
    }
    Ok(json!({
        "changed": changes.iter().map(|(i, c)| json!({"item": i.0, "colorSpace": c.id()})).collect::<Vec<_>>(),
        "proxiesAttached": proxies.iter().map(|(i, p)| json!({"item": i.0, "proxy": p})).collect::<Vec<_>>(),
        "warnings": warnings,
        "matched": matched,
        "keptExistingOverride": kept,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nlog_rules() -> Vec<InterpretRule> {
        validate(vec![
            InterpretRule { name: "N-RAW".into(), extensions: vec![".NEV".into()], color_space: "nlog-bt2020".into(), ..Default::default() },
            InterpretRule {
                name: "N-RAW proxy".into(),
                companion: Some(Companion { extension: "nev".into(), dirs: vec!["".into(), "../NRAW".into(), "/Volumes/Raw".into()] }),
                color_space: "nlog-bt2020".into(),
                ..Default::default()
            },
        ])
        .unwrap()
    }

    fn disk<'a>(entries: &'a [(&'a str, &'a [&'a str])]) -> impl Fn(&str) -> Option<Vec<String>> + 'a {
        move |dir| entries.iter().find(|(d, _)| *d == dir).map(|(_, f)| f.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn the_extension_rule_matches_in_any_case_and_path_style() {
        let r = nlog_rules();
        let none = disk(&[]);
        assert_eq!(color_space_for(&r, "/a/b/DSC_0275.NEV", &none), Some(ColorSpace::NLogBt2020));
        assert_eq!(color_space_for(&r, "C:\\clips\\dsc_0275.nev", &none), Some(ColorSpace::NLogBt2020));
        assert_eq!(color_space_for(&r, "/a/b/DSC_0275.MP4", &none), None);
    }

    #[test]
    fn a_proxy_matches_when_its_raw_clip_is_beside_it_or_in_a_listed_folder() {
        let r = nlog_rules();
        let beside = disk(&[("/card/DCIM", &["DSC_0275.MP4", "DSC_0275.NEV"])]);
        assert_eq!(color_space_for(&r, "/card/DCIM/DSC_0275.MP4", &beside), Some(ColorSpace::NLogBt2020));
        let sibling = disk(&[("/proj/proxies", &["DSC_0275.MP4"]), ("/proj/proxies/../NRAW", &["dsc_0275.nev"])]);
        assert_eq!(color_space_for(&r, "/proj/proxies/DSC_0275.MP4", &sibling), Some(ColorSpace::NLogBt2020));
        let absolute = disk(&[("/Volumes/Raw", &["DSC_0275.NEV"])]);
        assert_eq!(color_space_for(&r, "/elsewhere/DSC_0275.MP4", &absolute), Some(ColorSpace::NLogBt2020));
    }

    #[test]
    fn a_clip_without_its_raw_clip_is_left_alone() {
        let r = nlog_rules();
        let other = disk(&[("/card/DCIM", &["DSC_0275.MP4", "DSC_0276.NEV", "DSC_0275.MOV"])]);
        assert_eq!(color_space_for(&r, "/card/DCIM/DSC_0275.MP4", &other), None);
        // a folder that cannot be listed finds nothing
        assert_eq!(color_space_for(&r, "/card/DCIM/DSC_0275.MP4", &disk(&[])), None);
    }

    #[test]
    fn hidden_and_appledouble_files_are_not_companions() {
        let r = nlog_rules();
        let d = disk(&[("/card", &["._DSC_0275.NEV", "DSC_0275.MP4"])]);
        assert_eq!(color_space_for(&r, "/card/DSC_0275.MP4", &d), None);
    }

    #[test]
    fn the_first_matching_rule_wins_and_unknown_spaces_are_skipped() {
        let mut r = vec![
            InterpretRule { extensions: vec!["mp4".into()], color_space: "no-such-space".into(), ..Default::default() },
            InterpretRule { extensions: vec!["mp4".into()], color_space: "rec709".into(), ..Default::default() },
            InterpretRule { extensions: vec!["mp4".into()], color_space: "nlog-bt2020".into(), ..Default::default() },
        ];
        assert_eq!(color_space_for(&r, "a.mp4", &disk(&[])), Some(ColorSpace::Rec709));
        r.remove(1);
        assert_eq!(color_space_for(&r, "a.mp4", &disk(&[])), Some(ColorSpace::NLogBt2020));
    }

    #[test]
    fn odd_paths_never_panic() {
        let r = nlog_rules();
        let d = disk(&[]);
        for p in ["", ".", "..", "/", "\\", ".NEV", "a.", "/.mp4", "x/.NEV", "é/ü.nev", "C:", "C:\\"] {
            let _ = color_space_for(&r, p, &d);
        }
        // a rule that lists the clip's own extension as the companion does not match the clip itself
        let own = validate(vec![InterpretRule {
            companion: Some(Companion { extension: "mp4".into(), dirs: vec![] }),
            color_space: "rec709".into(),
            ..Default::default()
        }])
        .unwrap();
        assert_eq!(color_space_for(&own, "/a/x.mp4", &disk(&[("/a", &["x.mp4"])])), None);
    }

    fn proxy_rules() -> Vec<InterpretRule> {
        validate(vec![InterpretRule {
            name: "N-RAW".into(),
            extensions: vec!["NEV".into()],
            proxy: Some(ProxyLink { extension: ".MP4".into(), dirs: vec!["".into(), "Proxy".into(), "../Proxy".into()] }),
            color_space: "nlog-bt2020".into(),
            ..Default::default()
        }])
        .unwrap()
    }

    #[test]
    fn the_proxy_is_found_by_name_in_the_listed_folders() {
        let r = proxy_rules();
        // the job layout: the clip in Nikon/, its proxy in Nikon/Proxy/
        let job = disk(&[("/job/Nikon", &["DSC_0275.NEV", "DSC_0276.NEV"]), ("/job/Nikon/Proxy", &["DSC_0275.MP4", "dsc_0276.mp4", "._DSC_0275.MP4"])]);
        assert_eq!(proxy_for(&r, "/job/Nikon/DSC_0275.NEV", &job).as_deref(), Some("/job/Nikon/Proxy/DSC_0275.MP4"));
        assert_eq!(
            proxy_for(&r, "/job/Nikon/DSC_0276.NEV", &job).as_deref(),
            Some("/job/Nikon/Proxy/dsc_0276.mp4"),
            "any case, and the real file name is kept"
        );
        // the card layout: both in one folder
        let card = disk(&[("/card/DCIM", &["DSC_0275.NEV", "DSC_0275.MP4"])]);
        assert_eq!(proxy_for(&r, "/card/DCIM/DSC_0275.NEV", &card).as_deref(), Some("/card/DCIM/DSC_0275.MP4"));
        // a proxy folder beside the clip's folder: `..` is resolved in the result
        let sibling = disk(&[("/job/Raw", &["DSC_0275.NEV"]), ("/job/Raw/../Proxy", &["DSC_0275.MP4"])]);
        assert_eq!(proxy_for(&r, "/job/Raw/DSC_0275.NEV", &sibling).as_deref(), Some("/job/Proxy/DSC_0275.MP4"));
    }

    #[test]
    fn no_proxy_is_found_when_there_is_none_or_it_is_not_for_this_clip() {
        let r = proxy_rules();
        let job = disk(&[("/job/Nikon", &["DSC_0275.NEV"]), ("/job/Nikon/Proxy", &["DSC_0999.MP4", "._DSC_0275.MP4", "DSC_0275.MOV"])]);
        assert_eq!(proxy_for(&r, "/job/Nikon/DSC_0275.NEV", &job), None);
        assert_eq!(proxy_for(&r, "/job/Nikon/DSC_0275.NEV", &disk(&[])), None, "folders that cannot be listed find nothing");
        // a clip no rule matches has no proxy, whatever is beside it
        let other = disk(&[("/job", &["a.mov"]), ("/job/Proxy", &["a.mp4"])]);
        assert_eq!(proxy_for(&r, "/job/a.mov", &other), None);
        // a rule without a proxy link never gives one
        let plain = validate(vec![InterpretRule { extensions: vec!["nev".into()], color_space: "nlog-bt2020".into(), ..Default::default() }]).unwrap();
        let job = disk(&[("/job/Nikon", &["DSC_0275.NEV"]), ("/job/Nikon/Proxy", &["DSC_0275.MP4"])]);
        assert_eq!(proxy_for(&plain, "/job/Nikon/DSC_0275.NEV", &job), None);
    }

    #[test]
    fn a_clip_is_never_its_own_proxy() {
        let same = validate(vec![InterpretRule {
            extensions: vec!["mp4".into()],
            proxy: Some(ProxyLink { extension: "mp4".into(), dirs: vec!["".into()] }),
            color_space: "rec709".into(),
            ..Default::default()
        }])
        .unwrap();
        assert_eq!(proxy_for(&same, "/a/x.mp4", &disk(&[("/a", &["x.mp4"])])), None);
        assert_eq!(proxy_for(&same, "/a/x.mp4", &disk(&[("/a", &["x.mp4"]), ("/a", &["x.mp4"])])), None);
    }

    #[test]
    fn odd_paths_never_panic_when_looking_for_a_proxy() {
        let r = proxy_rules();
        let d = disk(&[]);
        for p in ["", ".", "..", "/", "\\", ".NEV", "a.", "/.nev", "x/.NEV", "é/ü.nev", "C:", "C:\\", "a/b/../../../c.nev"] {
            let _ = proxy_for(&r, p, &d);
        }
    }

    #[test]
    fn tidy_path_resolves_dot_segments_only_for_slash_paths() {
        assert_eq!(tidy_path("/a/b/../c"), "/a/c");
        assert_eq!(tidy_path("/a/./b"), "/a/b");
        assert_eq!(tidy_path("/a/b/../../c"), "/c");
        assert_eq!(tidy_path("a/../../b"), "../b", "a leading .. stays");
        assert_eq!(tidy_path("C:\\a\\..\\b"), "C:\\a\\..\\b", "a backslash path is left alone");
        assert_eq!(tidy_path(""), "");
    }

    #[test]
    fn validation_normalises_a_proxy_link_and_rejects_a_bad_one() {
        let ok = proxy_rules();
        assert_eq!(ok[0].proxy.as_ref().unwrap().extension, "mp4");
        let none = InterpretRule { extensions: vec!["nev".into()], proxy: Some(ProxyLink::default()), color_space: "rec709".into(), ..Default::default() };
        assert!(validate(vec![none]).unwrap_err().contains("proxy needs an extension"));
        let many = InterpretRule {
            extensions: vec!["nev".into()],
            proxy: Some(ProxyLink { extension: "mp4".into(), dirs: vec![String::new(); MAX_COMPANION_DIRS + 1] }),
            color_space: "rec709".into(),
            ..Default::default()
        };
        assert!(validate(vec![many]).is_err());
    }

    #[test]
    fn validation_rejects_bad_rules_and_normalises_extensions() {
        let ok =
            validate(vec![InterpretRule { extensions: vec![" .NeV ".into(), "".into()], color_space: "nlog-bt2020".into(), ..Default::default() }]).unwrap();
        assert_eq!(ok[0].extensions, vec!["nev".to_string()]);
        let bad_cs = InterpretRule { extensions: vec!["nev".into()], color_space: "nope".into(), ..Default::default() };
        assert!(validate(vec![bad_cs]).unwrap_err().contains("unknown colour space"));
        let empty = InterpretRule { color_space: "rec709".into(), ..Default::default() };
        assert!(validate(vec![empty]).unwrap_err().contains("extensions"));
        let no_ext = InterpretRule { companion: Some(Companion::default()), color_space: "rec709".into(), ..Default::default() };
        assert!(validate(vec![no_ext]).unwrap_err().contains("extension"));
        let many = vec![InterpretRule { extensions: vec!["a".into()], color_space: "rec709".into(), ..Default::default() }; MAX_RULES + 1];
        assert!(validate(many).is_err());
        let dirs = InterpretRule {
            companion: Some(Companion { extension: "nev".into(), dirs: vec![String::new(); MAX_COMPANION_DIRS + 1] }),
            color_space: "rec709".into(),
            ..Default::default()
        };
        assert!(validate(vec![dirs]).is_err());
    }
}
