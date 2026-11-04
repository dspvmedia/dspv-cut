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
//! sibling folder), or when the file itself carries a *tag*: a piece of text inside one box of an
//! MP4 / QuickTime file (`moov/udta/<maker box>`), where cameras write what they were set to (a
//! log recording that is not written as raw has no companion, but its camera says so in the file).
//! The first matching rule wins and sets the clip's colour space exactly as
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
/// Deepest box path a tag may name.
const MAX_TAG_DEPTH: usize = 8;
/// Longest text a tag may look for.
const MAX_TAG_TEXT: usize = 256;
/// Most boxes looked at on one level (a file of a million tiny boxes must not stall an import).
const MAX_TAG_BOXES: usize = 4096;
/// Most bytes of the named box that are searched.
const MAX_TAG_BYTES: u64 = 4 << 20;

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

/// Text the file carries in one box of an ISO base media / QuickTime file (MP4, MOV): `path` names the box by its four-character
/// types from the top level, separated by `/` (e.g. `moov/udta/NCDT`, where a camera keeps its maker notes), and `contains` is text
/// that must occur somewhere in that box (exact bytes, case-sensitive; only the first 4 MB of the box are searched). Boxes on the
/// way are read as plain containers. A file that is not such a file, or has no such box, does not match.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FileTag {
    pub path: String,
    pub contains: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct InterpretRule {
    pub name: String,
    /// Match by the clip's own extension (lower case, no dot).
    pub extensions: Vec<String>,
    /// Match by a companion file.
    pub companion: Option<Companion>,
    /// Match by text inside the file's own metadata.
    pub tag: Option<FileTag>,
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

/// Reads up to `len` bytes of the clip's file from `offset` (`None`: cannot be read).
type ReadAt<'a> = &'a dyn Fn(u64, usize) -> Option<Vec<u8>>;

/// The four-character box types of a tag path (`None`: not a valid path).
fn tag_path(path: &str) -> Option<Vec<[u8; 4]>> {
    let parts: Vec<[u8; 4]> = path.split('/').map(|p| p.as_bytes().try_into().ok()).collect::<Option<_>>()?;
    (!parts.is_empty() && parts.len() <= MAX_TAG_DEPTH).then_some(parts)
}

/// The bytes of the box at `path` (at most [`MAX_TAG_BYTES`] of them), walking box headers with `read`.
fn box_bytes(read: ReadAt<'_>, path: &[[u8; 4]]) -> Option<Vec<u8>> {
    let (mut start, mut end) = (0u64, u64::MAX);
    for want in path {
        let mut off = start;
        let mut found = None;
        for _ in 0..MAX_TAG_BOXES {
            if off.checked_add(8).is_none_or(|h| h > end) {
                break;
            }
            let head = read(off, 16)?;
            let (Some(size), Some(kind)) = (head.get(0..4), head.get(4..8)) else { break };
            let (size, header) = match u32::from_be_bytes(size.try_into().ok()?) {
                // 64-bit size follows the type
                1 => (u64::from_be_bytes(head.get(8..16)?.try_into().ok()?), 16u64),
                // the box runs to the end of its parent (or of the file)
                0 => (end.saturating_sub(off), 8),
                n => (u64::from(n), 8),
            };
            if size < header {
                break;
            }
            let box_end = off.saturating_add(size).min(end);
            if kind == want {
                found = Some((off.saturating_add(header), box_end));
                break;
            }
            if box_end <= off {
                break;
            }
            off = box_end;
        }
        (start, end) = found?;
    }
    let len = end.saturating_sub(start).min(MAX_TAG_BYTES);
    read(start, usize::try_from(len).ok()?)
}

/// Whether the file read by `read` carries `tag`.
fn has_tag(tag: &FileTag, read: ReadAt<'_>) -> bool {
    let needle = tag.contains.as_bytes();
    let Some(path) = tag_path(&tag.path) else { return false };
    !needle.is_empty() && box_bytes(read, &path).is_some_and(|b| b.windows(needle.len()).any(|w| w == needle))
}

/// Whether `rule` matches a clip called `<stem>.<ext>` in `media_dir`: by its own extension, by a companion file, or by a tag
/// the file carries (read with `read`).
fn rule_matches(rule: &InterpretRule, ext: &str, stem: &str, media_dir: &str, list: &dyn Fn(&str) -> Option<Vec<String>>, read: ReadAt<'_>) -> bool {
    if !ext.is_empty() && rule.extensions.iter().any(|e| e.trim_start_matches('.').eq_ignore_ascii_case(ext)) {
        return true;
    }
    if rule.tag.as_ref().is_some_and(|t| has_tag(t, read)) {
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
/// colour space id is unknown is skipped, never an error: rules come from a file. Tags are not
/// read here (see [`color_space_for_file`]).
pub fn color_space_for(rules: &[InterpretRule], path: &str, list: &dyn Fn(&str) -> Option<Vec<String>>) -> Option<ColorSpace> {
    color_space_for_file(rules, path, list, &|_, _| None)
}

/// [`color_space_for`], reading the file with `read` (offset, length) for rules that match by a tag.
pub fn color_space_for_file(rules: &[InterpretRule], path: &str, list: &dyn Fn(&str) -> Option<Vec<String>>, read: ReadAt<'_>) -> Option<ColorSpace> {
    let (media_dir, file) = split_path(path);
    let (stem, ext) = split_name(file);
    if stem.is_empty() {
        return None;
    }
    rules.iter().take(MAX_RULES).find_map(|rule| {
        let cs = ColorSpace::parse(&rule.color_space)?;
        rule_matches(rule, ext, stem, media_dir, list, read).then_some(cs)
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
/// path; whether the proxy fits the clip is for the caller to check. Tags are not read here (see [`proxy_for_file`]).
pub fn proxy_for(rules: &[InterpretRule], path: &str, list: &dyn Fn(&str) -> Option<Vec<String>>) -> Option<String> {
    proxy_for_file(rules, path, list, &|_, _| None)
}

/// [`proxy_for`], reading the file with `read` (offset, length) for rules that match by a tag.
pub fn proxy_for_file(rules: &[InterpretRule], path: &str, list: &dyn Fn(&str) -> Option<Vec<String>>, read: ReadAt<'_>) -> Option<String> {
    let (media_dir, file) = split_path(path);
    let (stem, ext) = split_name(file);
    if stem.is_empty() {
        return None;
    }
    for rule in rules.iter().take(MAX_RULES) {
        let Some(link) = &rule.proxy else { continue };
        let link_ext = link.extension.trim_start_matches('.');
        if link_ext.is_empty() || !rule_matches(rule, ext, stem, media_dir, list, read) {
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

/// [`color_space_for_file`] with the host's directory listing and file reads.
pub fn resolve(rules: &[InterpretRule], path: &str, services: &dyn Services) -> Option<ColorSpace> {
    if rules.is_empty() {
        return None;
    }
    color_space_for_file(rules, path, &|dir| services.list_dir(dir.trim_end_matches(['/', '\\'])).and_then(|r| r.ok()), &|off, len| {
        services.read_range(path, off, len).ok()
    })
}

/// [`proxy_for_file`] with the host's directory listing and file reads.
pub fn find_proxy(rules: &[InterpretRule], path: &str, services: &dyn Services) -> Option<String> {
    if rules.is_empty() {
        return None;
    }
    proxy_for_file(rules, path, &|dir| services.list_dir(dir.trim_end_matches(['/', '\\'])).and_then(|r| r.ok()), &|off, len| {
        services.read_range(path, off, len).ok()
    })
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
        if let Some(t) = r.tag.as_mut() {
            t.path = t.path.trim().trim_matches('/').to_string();
            if tag_path(&t.path).is_none() {
                return Err(at(&format!("a tag path is up to {MAX_TAG_DEPTH} four-character box types separated by `/`, e.g. `moov/udta/NCDT`")));
            }
            if t.contains.is_empty() || t.contains.len() > MAX_TAG_TEXT {
                return Err(at(&format!("a tag needs `contains`: 1 to {MAX_TAG_TEXT} bytes of text")));
            }
        }
        if r.extensions.is_empty() && r.companion.is_none() && r.tag.is_none() {
            return Err(at("needs `extensions`, a `companion` or a `tag`"));
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

    /// A box: 32-bit size, type, payload.
    fn bx(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut b = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(kind);
        b.extend_from_slice(payload);
        b
    }

    /// An MP4 laid out like a Nikon camera file: `ftyp`, then `moov` holding `mvhd`, a `trak` and `udta/NCDT` (the maker notes,
    /// where the camera writes its Picture Control name), then `mdat`.
    fn camera_file(picture_control: &str) -> Vec<u8> {
        let mut notes = b"\x00\x23\x00\x07\x00\x00\x00\x6c0310".to_vec();
        notes.extend_from_slice(picture_control.as_bytes());
        notes.extend_from_slice(&[0; 20]);
        let udta = bx(b"udta", &[bx(b"NCTG", b"NIKON Z 9"), bx(b"NCDT", &notes)].concat());
        let moov = bx(b"moov", &[bx(b"mvhd", &[0; 100]), bx(b"trak", &bx(b"tkhd", &[0; 84])), udta].concat());
        [bx(b"ftyp", b"mp42\x00\x00\x00\x01mp42avc1niko"), moov, bx(b"mdat", &[0xAB; 3000])].concat()
    }

    fn reader(bytes: &[u8]) -> impl Fn(u64, usize) -> Option<Vec<u8>> + '_ {
        move |off, len| {
            let a = usize::try_from(off).ok()?.min(bytes.len());
            Some(bytes[a..a.saturating_add(len).min(bytes.len())].to_vec())
        }
    }

    fn tag_rules() -> Vec<InterpretRule> {
        validate(vec![InterpretRule {
            name: "camera log".into(),
            tag: Some(FileTag { path: "/moov/udta/NCDT/".into(), contains: "N-LOG".into() }),
            color_space: "nlog-bt2020".into(),
            ..Default::default()
        }])
        .unwrap()
    }

    #[test]
    fn a_file_whose_camera_says_log_matches_and_one_that_says_otherwise_does_not() {
        let none = |_: &str| None;
        let log = camera_file("N-LOG");
        let neutral = camera_file("NEUTRAL");
        let rules = tag_rules();
        assert_eq!(rules[0].tag.as_ref().unwrap().path, "moov/udta/NCDT", "slashes at the ends are trimmed");
        assert_eq!(color_space_for_file(&rules, "/clips/DSC_0303.MP4", &none, &reader(&log)), Some(ColorSpace::NLogBt2020));
        assert_eq!(color_space_for_file(&rules, "/clips/DSC_0092.MP4", &none, &reader(&neutral)), None);
        // without file reads (the old entry point) a tag never matches
        assert_eq!(color_space_for(&rules, "/clips/DSC_0303.MP4", &none), None);
    }

    #[test]
    fn the_text_must_be_in_the_named_box_not_elsewhere_in_the_file() {
        let none = |_: &str| None;
        // "N-LOG" in mdat (picture data) and in another udta box, but the maker notes say NEUTRAL
        let mut f = camera_file("NEUTRAL");
        f.extend_from_slice(&bx(b"free", b"N-LOG"));
        let udta = bx(b"udta", &bx(b"XTRA", b"N-LOG"));
        let moov = bx(b"moov", &udta);
        f.extend_from_slice(&moov);
        assert_eq!(color_space_for_file(&tag_rules(), "a.mp4", &none, &reader(&f)), None);
        // a path to a box that is not there
        let other = validate(vec![InterpretRule {
            tag: Some(FileTag { path: "moov/meta/NCDT".into(), contains: "N-LOG".into() }),
            color_space: "nlog-bt2020".into(),
            ..Default::default()
        }])
        .unwrap();
        assert_eq!(color_space_for_file(&other, "a.mp4", &none, &reader(&camera_file("N-LOG"))), None);
    }

    #[test]
    fn large_sizes_and_boxes_that_run_to_the_end_are_followed() {
        let none = |_: &str| None;
        // moov written with a 64-bit size, and maker notes in a size-0 box (runs to the end of its parent)
        let notes = b"....N-LOG....".to_vec();
        let mut ncdt = 0u32.to_be_bytes().to_vec();
        ncdt.extend_from_slice(b"NCDT");
        ncdt.extend_from_slice(&notes);
        let udta = bx(b"udta", &ncdt);
        let mut moov = 1u32.to_be_bytes().to_vec();
        moov.extend_from_slice(b"moov");
        moov.extend_from_slice(&((udta.len() + 16) as u64).to_be_bytes());
        moov.extend_from_slice(&udta);
        let f = [bx(b"ftyp", b"isom"), moov].concat();
        assert_eq!(color_space_for_file(&tag_rules(), "a.mov", &none, &reader(&f)), Some(ColorSpace::NLogBt2020));
    }

    #[test]
    fn files_that_are_not_boxes_or_are_broken_never_panic_and_never_match() {
        let none = |_: &str| None;
        let rules = tag_rules();
        let wav = b"RIFF\x24\x00\x00\x00WAVEfmt ".to_vec();
        let mut cut = camera_file("N-LOG");
        cut.truncate(60);
        let tiny = bx(b"moov", &[1, 2, 3]);
        let mut zero_size_loop = 8u32.to_be_bytes().to_vec();
        zero_size_loop.extend_from_slice(b"free");
        zero_size_loop = zero_size_loop.repeat(MAX_TAG_BOXES + 10);
        let mut lies = u32::MAX.to_be_bytes().to_vec();
        lies.extend_from_slice(b"moov");
        let mut short = 3u32.to_be_bytes().to_vec();
        short.extend_from_slice(b"moov");
        let mut huge64 = 1u32.to_be_bytes().to_vec();
        huge64.extend_from_slice(b"moov");
        huge64.extend_from_slice(&u64::MAX.to_be_bytes());
        for f in [vec![], vec![0; 3], wav, cut, tiny, zero_size_loop, lies, short, huge64] {
            assert_eq!(color_space_for_file(&rules, "a.mp4", &none, &reader(&f)), None);
        }
        // an unreadable file
        assert_eq!(color_space_for_file(&rules, "a.mp4", &none, &|_, _| None), None);
    }

    #[test]
    fn a_tag_rule_can_name_a_proxy_too() {
        let log = camera_file("N-LOG");
        let rules = validate(vec![InterpretRule {
            tag: Some(FileTag { path: "moov/udta/NCDT".into(), contains: "N-LOG".into() }),
            proxy: Some(ProxyLink { extension: "mp4".into(), dirs: vec!["Proxy".into()] }),
            color_space: "nlog-bt2020".into(),
            ..Default::default()
        }])
        .unwrap();
        let list = disk(&[("/job/Proxy", &["DSC_1.MP4"])]);
        assert_eq!(proxy_for_file(&rules, "/job/DSC_1.MOV", &list, &reader(&log)).as_deref(), Some("/job/Proxy/DSC_1.MP4"));
        assert_eq!(proxy_for_file(&rules, "/job/DSC_1.MOV", &list, &reader(&camera_file("STANDARD"))), None);
    }

    #[test]
    fn validation_checks_tags() {
        let with = |path: &str, contains: &str| {
            validate(vec![InterpretRule {
                tag: Some(FileTag { path: path.into(), contains: contains.into() }),
                color_space: "nlog-bt2020".into(),
                ..Default::default()
            }])
        };
        assert!(with("moov/udta/NCDT", "N-LOG").is_ok());
        for bad in ["", "moov//NCDT", "moov/udta/NCD", "moov/udta/NCDTX", &"moov/".repeat(MAX_TAG_DEPTH + 1)] {
            assert!(with(bad, "N-LOG").unwrap_err().contains("tag path"), "{bad:?}");
        }
        assert!(with("moov", "").unwrap_err().contains("contains"));
        assert!(with("moov", &"x".repeat(MAX_TAG_TEXT + 1)).unwrap_err().contains("contains"));
        let empty = InterpretRule { color_space: "rec709".into(), ..Default::default() };
        assert!(validate(vec![empty]).unwrap_err().contains("tag"));
    }
}
