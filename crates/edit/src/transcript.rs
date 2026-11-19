//! Text-based editing: the sequence transcript and the edits made through it.
//!
//! Clip transcripts (`filmcraft_project::Transcript`) hold words in **media time**. The sequence
//! transcript ([`sequence_words`]) maps them through the audio track items that play them:
//!
//! - audio tracks are read top first (A1, A2…); a word is taken from the first track whose
//!   transcribed clip covers the word's midpoint, so a dialogue clip duplicated on two tracks (or a
//!   stereo pair split over two mono tracks) reads once;
//! - a word belongs to a clip when its midpoint, mapped through the clip's speed, falls inside the
//!   clip; its timeline bounds are clamped to the clip, so a word cut by an edit is shown cut;
//! - disabled clips, reversed clips and frame holds contribute no words (they don't play speech).
//!
//! Text edits turn word ranges into timeline ranges ([`word_range`]) snapped outward to frames, then
//! reuse the ordinary [`crate::extract`] / [`crate::lift`] edits. Pause and filler-word removal
//! ([`find_pauses`], [`find_fillers`]) produce many ranges that [`ripple_delete_ranges`] removes in
//! one pass, right to left. Captions come from [`caption_blocks`].

use std::collections::BTreeMap;
use std::sync::Arc;

use filmcraft_project::{Caption, ClipId, ItemId, Sequence, TrackId, Transcript};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange};

use crate::EditCtx;

/// A word of the sequence transcript.
#[derive(Clone, Debug, PartialEq)]
pub struct SeqWord {
    pub text: String,
    /// Sequence time.
    pub start: Tick,
    pub end: Tick,
    /// The track item it is heard through and that item's media.
    pub clip: ClipId,
    pub item: ItemId,
    /// Index of the word in the media item's transcript.
    pub index: usize,
    /// Audio track index (0 = A1).
    pub track: usize,
    pub speaker: Option<String>,
    pub confidence: f32,
}

impl SeqWord {
    pub fn normalized(&self) -> String {
        filmcraft_project::transcript::normalize_word(&self.text)
    }
    pub fn range(&self) -> TimeRange {
        TimeRange::from_bounds(self.start, self.end.max(self.start))
    }
}

/// Transcripts by media item (as in `Project::transcripts`).
pub type Transcripts = BTreeMap<ItemId, Arc<Transcript>>;

fn ticks(t: f64) -> Tick {
    Tick(t.round() as i64)
}

/// The sequence transcript: words of every transcribed clip on the audio tracks, in sequence
/// time order (see the module docs for the rules).
pub fn sequence_words(seq: &Sequence, transcripts: &Transcripts) -> Vec<SeqWord> {
    let mut out: Vec<SeqWord> = Vec::new();
    // timeline ranges already served by a higher track's transcribed clips
    let mut claimed = Coverage::default();
    for (ti, track) in seq.audio_tracks.iter().enumerate() {
        let mut mine = Vec::new();
        for it in &track.items {
            if !it.enabled || it.reverse || it.frame_hold.is_some() || it.speed <= 0.0 {
                continue;
            }
            let Some(tr) = transcripts.get(&it.item) else { continue };
            mine.push(it.range());
            let speed = it.speed;
            let media_end = it.source_in + ticks(it.duration.0 as f64 * speed);
            let to_tl = |m: Tick| it.start + ticks((m - it.source_in).0 as f64 / speed);
            for wi in tr.words_in(TimeRange::from_bounds(it.source_in, media_end.max(it.source_in))) {
                let w = &tr.words[wi];
                let (a, b) = (to_tl(w.start), to_tl(w.end.max(w.start)));
                let mid = Tick(a.0 + (b.0 - a.0) / 2);
                if mid < it.start || mid >= it.end() || claimed.contains(mid) {
                    continue;
                }
                out.push(SeqWord {
                    text: w.text.clone(),
                    start: a.max(it.start),
                    end: b.min(it.end()).max(a.max(it.start)),
                    clip: it.id,
                    item: it.item,
                    index: wi,
                    track: ti,
                    speaker: tr.speaker_name(w),
                    confidence: w.confidence,
                });
            }
        }
        claimed.add(mine);
    }
    out.sort_by_key(|w| (w.start, w.track));
    out
}

/// Sorted, disjoint, non-empty `[start, end)` ranges: a point lookup is a binary search, so a
/// project with many clips and words is not checked range by range for every word.
#[derive(Debug, Default)]
struct Coverage(Vec<(Tick, Tick)>);

impl Coverage {
    /// Merge `ranges` in (empty and negative ones cover nothing).
    fn add(&mut self, ranges: Vec<TimeRange>) {
        let mut all: Vec<(Tick, Tick)> = std::mem::take(&mut self.0);
        all.extend(ranges.into_iter().filter(|r| r.end() > r.start).map(|r| (r.start, r.end())));
        all.sort_unstable();
        for (a, b) in all {
            match self.0.last_mut() {
                Some(last) if a <= last.1 => last.1 = last.1.max(b),
                _ => self.0.push((a, b)),
            }
        }
    }

    fn contains(&self, t: Tick) -> bool {
        let i = self.0.partition_point(|r| r.0 <= t);
        i.checked_sub(1).and_then(|i| self.0.get(i)).is_some_and(|r| t < r.1)
    }
}

/// Index of the word being spoken at `t` (the last word starting at or before `t` whose end is
/// after `t`).
pub fn word_at(words: &[SeqWord], t: Tick) -> Option<usize> {
    let i = words.partition_point(|w| w.start <= t).checked_sub(1)?;
    (t < words[i].end).then_some(i)
}

/// Paragraphs (Text panel segments): runs of words split where the speaker changes or at a pause
/// of at least `gap`.
pub fn paragraphs(words: &[SeqWord], gap: Tick) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut a = 0;
    for i in 1..=words.len() {
        if i == words.len() || words[i].speaker != words[i - 1].speaker || words[i].start - words[i - 1].end >= gap {
            if a < i {
                out.push(a..i);
            }
            a = i;
        }
    }
    out
}

/// Matches of `query` (one or more words, case and punctuation ignored; the last query word may be
/// a prefix) as word index ranges.
pub fn search(words: &[SeqWord], query: &str) -> Vec<std::ops::Range<usize>> {
    let q: Vec<String> = query.split_whitespace().map(filmcraft_project::transcript::normalize_word).filter(|s| !s.is_empty()).collect();
    if q.is_empty() {
        return Vec::new();
    }
    let norm: Vec<String> = words.iter().map(SeqWord::normalized).collect();
    let m = q.len();
    // The leading query words must match whole words; the last may be a prefix. Find every
    // occurrence of the leading words in one pass (KMP: a mismatch never restarts the query).
    let (lead, last) = (&q[..m - 1], &q[m - 1]);
    let mut fail = vec![0usize; lead.len()];
    for i in 1..lead.len() {
        let mut k = fail[i - 1];
        while k > 0 && lead[i] != lead[k] {
            k = fail[k - 1];
        }
        fail[i] = k + usize::from(lead[i] == lead[k]);
    }
    let mut starts = Vec::new();
    if lead.is_empty() {
        starts.extend(0..norm.len());
    } else {
        let mut k = 0;
        for (j, w) in norm.iter().enumerate() {
            while k > 0 && *w != lead[k] {
                k = fail[k - 1];
            }
            k += usize::from(*w == lead[k]);
            if k == lead.len() {
                starts.push(j + 1 - k);
                k = fail[k - 1];
            }
        }
    }
    starts.into_iter().filter(|&i| norm.get(i + m - 1).is_some_and(|w| w.starts_with(last.as_str()))).map(|i| i..i + m).collect()
}

/// Timeline range of words `a..=b`, snapped outward to frames.
pub fn word_range(words: &[SeqWord], a: usize, b: usize, rate: FrameRate) -> Option<TimeRange> {
    let (a, b) = (a.min(b), a.max(b));
    let (s, e) = (words.get(a)?.start, words.get(b)?.end);
    let s = rate.snap(s);
    let mut e2 = rate.snap(e);
    if e2 < e || e2 <= s {
        e2 += rate.frame_duration();
    }
    Some(TimeRange::from_bounds(s, e2))
}

/// Media range of words `a..=b` of a clip transcript, snapped outward to the media's frames (for
/// Source-monitor In/Out from a text selection).
pub fn media_word_range(t: &Transcript, a: usize, b: usize, rate: FrameRate) -> Option<TimeRange> {
    let (a, b) = (a.min(b), a.max(b));
    let (s, e) = (t.words.get(a)?.start, t.words.get(b)?.end);
    let s = rate.snap(s);
    let mut e2 = rate.snap(e);
    if e2 < e || e2 <= s {
        e2 += rate.frame_duration();
    }
    Some(TimeRange::from_bounds(s, e2))
}

/// Pauses between consecutive words of at least `min`, as the ranges to remove: each keeps `keep`
/// of silence next to both words and is snapped inward to frames (pauses shorter than one frame
/// after that are skipped).
pub fn find_pauses(words: &[SeqWord], min: Tick, keep: Tick, rate: FrameRate) -> Vec<TimeRange> {
    let mut out = Vec::new();
    for p in words.windows(2) {
        let (a, b) = (p[0].end, p[1].start);
        if b - a < min || b <= a {
            continue;
        }
        let s = a + keep;
        let e = b - keep;
        let mut s2 = rate.snap(s);
        if s2 < s {
            s2 += rate.frame_duration();
        }
        let e2 = rate.snap(e);
        if e2 > s2 {
            out.push(TimeRange::from_bounds(s2, e2));
        }
    }
    out
}

/// The default filler words and phrases (configurable in preferences and per command).
pub const DEFAULT_FILLERS: &[&str] = &["um", "uh", "umm", "uhm", "erm", "er", "ah", "hmm", "mm", "mhm"];

/// Filler words: word index ranges matching one of `fillers` (each a word or a phrase such as
/// "you know"; case and punctuation ignored).
pub fn find_fillers(words: &[SeqWord], fillers: &[String]) -> Vec<std::ops::Range<usize>> {
    let norm: Vec<String> = words.iter().map(SeqWord::normalized).collect();
    let mut phrases: Vec<Vec<String>> = fillers
        .iter()
        .map(|f| f.split_whitespace().map(filmcraft_project::transcript::normalize_word).filter(|s| !s.is_empty()).collect::<Vec<_>>())
        .filter(|p| !p.is_empty())
        .collect();
    // longest phrases first, so "you know" wins over a lone "you"
    phrases.sort_by_key(|p| std::cmp::Reverse(p.len()));
    let mut out = Vec::new();
    let mut i = 0;
    while i < norm.len() {
        let hit = phrases.iter().find(|p| i + p.len() <= norm.len() && p.iter().enumerate().all(|(k, w)| norm[i + k] == *w));
        match hit {
            Some(p) => {
                out.push(i..i + p.len());
                i += p.len();
            }
            None => i += 1,
        }
    }
    out
}

/// Timeline ranges removing the filler words `hits`: each word range snapped to the nearest frames
/// (never into the neighbouring words' frames).
pub fn filler_ranges(words: &[SeqWord], hits: &[std::ops::Range<usize>], rate: FrameRate) -> Vec<TimeRange> {
    let mut out = Vec::new();
    for h in hits {
        let (a, b) = (h.start, h.end - 1);
        let mut s = rate.snap_nearest(words[a].start);
        let mut e = rate.snap_nearest(words[b].end);
        if a > 0 && s < words[a - 1].end {
            s = rate.snap(words[a - 1].end) + rate.frame_duration();
        }
        if let Some(n) = words.get(b + 1)
            && e > n.start
        {
            e = rate.snap(n.start);
        }
        if e > s {
            out.push(TimeRange::from_bounds(s, e));
        }
    }
    out
}

/// Rules for finding retakes (a speaker stopping and starting the same line again).
#[derive(Clone, Debug, PartialEq)]
pub struct RetakeRules {
    /// Leading words the restart must repeat word for word (fillers skipped) before it counts.
    pub min_words: usize,
    /// Share of an unfinished attempt's words the restart repeats, in order (0..=1). 1.0 keeps
    /// only pure false starts; lower also catches a line said again with a word or two changed.
    pub min_coverage: f64,
    /// The same share for an attempt that ends a sentence (`.`, `!` or `?`, not `...`). Set higher
    /// than `min_coverage`: two finished sentences that share an opening ("It's close to schools.
    /// It's close to the park.") are usually meant, so they go only when said again nearly word for word.
    pub min_coverage_sentence: f64,
    /// Longest time from the start of the earlier attempt to the start of the restart.
    pub max_span: Tick,
    /// Words skipped when comparing (the filler list).
    pub fillers: Vec<String>,
}

impl Default for RetakeRules {
    fn default() -> Self {
        RetakeRules {
            min_words: 3,
            min_coverage: 0.8,
            min_coverage_sentence: 0.9,
            max_span: Tick(15 * TICKS_PER_SECOND),
            fillers: DEFAULT_FILLERS.iter().map(|f| f.to_string()).collect(),
        }
    }
}

/// Whether a word ends a sentence: `.`, `!` or `?` last (closing quotes and brackets ignored),
/// but not a trailing-off ellipsis (`...` or `…`).
pub fn ends_sentence(text: &str) -> bool {
    let t = text.trim_end_matches(['"', '\'', ')', ']', '”', '’']);
    !(t.ends_with("..") || t.ends_with('…')) && t.ends_with(['.', '!', '?'])
}

/// A retake: the earlier attempt `cut` (word indices, fillers inside it included) and the word
/// `restart` where the kept attempt begins (`cut.end == restart`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Retake {
    pub cut: std::ops::Range<usize>,
    pub restart: usize,
}

/// Longest common subsequence length of two word lists.
fn lcs(a: &[&str], b: &[&str]) -> usize {
    let mut row = vec![0usize; b.len() + 1];
    for x in a {
        let mut diag = 0;
        for (j, y) in b.iter().enumerate() {
            let up = row[j + 1];
            row[j + 1] = if x == y { diag + 1 } else { up.max(row[j]) };
            diag = up;
        }
    }
    row[b.len()]
}

/// Retakes, earliest first, never overlapping. An attempt is a retake when a later run of words —
/// same speaker, media item and track, starting within `max_span` — opens with the same
/// `min_words` words (fillers skipped) and repeats at least `min_coverage` of the attempt in order
/// (`min_coverage_sentence` when the attempt ends a sentence).
/// The last attempt is always the one kept: "I went to the, I went to the, I went to the store"
/// gives two retakes.
pub fn find_retakes(words: &[SeqWord], rules: &RetakeRules) -> Vec<Retake> {
    let min = rules.min_words.max(1);
    let mut filler = vec![false; words.len()];
    for h in find_fillers(words, &rules.fillers) {
        for k in h {
            filler[k] = true;
        }
    }
    // content words (indices into `words`), compared by their normalized text
    let norm: Vec<String> = words.iter().map(SeqWord::normalized).collect();
    let c: Vec<usize> = (0..words.len()).filter(|&k| !filler[k] && !norm[k].is_empty()).collect();
    let same_voice = |a: usize, b: usize| words[a].speaker == words[b].speaker && words[a].item == words[b].item && words[a].track == words[b].track;
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let mut found = None;
        for j in i + 1..c.len() {
            let (wi, wj) = (c[i], c[j]);
            if words[wj].start - words[wi].start > rules.max_span {
                break;
            }
            if !same_voice(wi, wj) {
                continue;
            }
            // the restart repeats the attempt's opening words (counted inside the attempt only)
            let lead = (0..).take_while(|&k| i + k < j && j + k < c.len() && norm[c[i + k]] == norm[c[j + k]]).count();
            if lead < min || (i..j).any(|k| !same_voice(c[k], wi)) {
                continue;
            }
            let attempt: Vec<&str> = c[i..j].iter().map(|&k| norm[k].as_str()).collect();
            let again: Vec<&str> = c[j..c.len().min(j + attempt.len() + 2)].iter().map(|&k| norm[k].as_str()).collect();
            let need = if ends_sentence(&words[c[j - 1]].text) { rules.min_coverage_sentence } else { rules.min_coverage };
            if lcs(&attempt, &again) as f64 >= need * attempt.len() as f64 {
                found = Some(j);
                break;
            }
        }
        match found {
            Some(j) => {
                out.push(Retake { cut: c[i]..c[j], restart: c[j] });
                i = j;
            }
            None => i += 1,
        }
    }
    out
}

/// Sort and merge ranges (overlapping or touching).
pub fn merge_ranges(mut r: Vec<TimeRange>) -> Vec<TimeRange> {
    r.sort_by_key(|x| x.start);
    let mut out: Vec<TimeRange> = Vec::new();
    for x in r {
        match out.last_mut() {
            Some(l) if x.start <= l.end() => *l = TimeRange::from_bounds(l.start, l.end().max(x.end())),
            _ => out.push(x),
        }
    }
    out
}

/// Ripple-delete every range on every unlocked track (and sync-locked caption tracks), right to
/// left so earlier ranges keep their positions. Returns the total time removed.
pub fn ripple_delete_ranges(seq: &mut Sequence, ranges: Vec<TimeRange>, ctx: &mut EditCtx) -> Tick {
    let tracks: Vec<TrackId> = seq.all_tracks().filter(|t| !t.locked).map(|t| t.id).collect();
    let mut total = Tick::ZERO;
    for r in merge_ranges(ranges).into_iter().rev() {
        if r.duration <= Tick::ZERO {
            continue;
        }
        crate::extract(seq, &tracks, r, ctx);
        total += r.duration;
    }
    total
}

/// Rules for Create Captions from a transcript (Premiere's dialog defaults).
#[derive(Clone, Debug, PartialEq)]
pub struct CaptionRules {
    /// Maximum characters per line.
    pub max_chars: usize,
    /// Lines per caption (1 = single, 2 = double).
    pub lines: usize,
    /// Minimum caption duration; a short caption is extended into the silence after it, never
    /// over the next one.
    pub min_duration: Tick,
    /// Longest caption.
    pub max_duration: Tick,
    /// Frames left empty between consecutive captions.
    pub gap_frames: i64,
    /// A pause at least this long starts a new caption.
    pub break_pause: Tick,
}

impl Default for CaptionRules {
    fn default() -> Self {
        Self {
            max_chars: 42,
            lines: 2,
            min_duration: Tick(TICKS_PER_SECOND),
            max_duration: Tick(7 * TICKS_PER_SECOND),
            gap_frames: 0,
            break_pause: Tick(TICKS_PER_SECOND),
        }
    }
}

/// A caption block made from words.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptionBlock {
    pub start: Tick,
    pub end: Tick,
    /// Lines joined by `\n`.
    pub text: String,
    pub speaker: Option<String>,
    /// The word index range it shows.
    pub words: std::ops::Range<usize>,
}

/// Lay words out as caption blocks: words fill lines of at most `max_chars` (a longer single word
/// gets a line of its own) and blocks of `lines` lines; a new block starts at a speaker change,
/// at a pause of `break_pause`, after `max_duration`, or after sentence-ending punctuation once the
/// block is past half full. Times are snapped to frames, blocks never overlap and keep
/// `gap_frames` between them.
pub fn caption_blocks(words: &[SeqWord], rules: &CaptionRules, rate: FrameRate) -> Vec<CaptionBlock> {
    let max_chars = rules.max_chars.max(1);
    let max_lines = rules.lines.max(1);
    let cap_chars = max_chars * max_lines;
    let mut groups: Vec<std::ops::Range<usize>> = Vec::new();
    let mut a = 0;
    let mut lines: Vec<usize> = vec![0];
    for i in 0..words.len() {
        let w = &words[i];
        let len = w.text.chars().count();
        if i > a {
            let prev = &words[i - 1];
            let cur = *lines.last().unwrap_or(&0);
            let fits_line = cur + 1 + len <= max_chars;
            let fits = fits_line || lines.len() < max_lines;
            let used: usize = lines.iter().sum::<usize>() + lines.len() - 1;
            let sentence_end = prev.text.ends_with(['.', '?', '!']) && used * 2 >= cap_chars;
            let brk =
                !fits || w.speaker != prev.speaker || w.start - prev.end >= rules.break_pause || w.end - words[a].start > rules.max_duration || sentence_end;
            if brk {
                groups.push(a..i);
                a = i;
                lines = vec![len];
                continue;
            }
            if fits_line {
                if let Some(l) = lines.last_mut() {
                    *l += 1 + len;
                }
            } else {
                lines.push(len);
            }
        } else {
            lines = vec![len];
        }
    }
    if a < words.len() {
        groups.push(a..words.len());
    }
    // text, frame-snapped times
    let fd = rate.frame_duration();
    let gap = Tick(fd.0 * rules.gap_frames.max(0));
    let mut out: Vec<CaptionBlock> = Vec::new();
    for g in groups {
        let mut text_lines: Vec<String> = vec![String::new()];
        for w in &words[g.clone()] {
            let Some(l) = text_lines.last_mut() else { break };
            if l.is_empty() {
                l.push_str(&w.text);
            } else if l.chars().count() + 1 + w.text.chars().count() <= max_chars {
                l.push(' ');
                l.push_str(&w.text);
            } else {
                text_lines.push(w.text.clone());
            }
        }
        let start = rate.snap(words[g.start].start);
        let mut end = rate.snap(words[g.end - 1].end);
        if end < words[g.end - 1].end {
            end += fd;
        }
        out.push(CaptionBlock { start, end: end.max(start + fd), text: text_lines.join("\n"), speaker: words[g.start].speaker.clone(), words: g });
    }
    // no overlaps, minimum duration (into the silence after a block, never over the next one)
    for i in 0..out.len() {
        if i > 0 {
            let min_start = out[i - 1].end + gap;
            if out[i].start < min_start {
                out[i].start = min_start;
                out[i].end = out[i].end.max(min_start + fd);
            }
        }
        let next = out.get(i + 1).map(|n| n.start);
        let b = &mut out[i];
        if b.end - b.start < rules.min_duration {
            let want = b.start + rules.min_duration;
            let mut e = rate.snap(want);
            if e < want {
                e += fd;
            }
            b.end = e;
        }
        if let Some(n) = next {
            b.end = b.end.min(n - gap).max(b.start + fd);
        }
    }
    out
}

/// Caption blocks as captions (ids from `ctx`).
pub fn blocks_to_captions(blocks: &[CaptionBlock], ctx: &mut EditCtx) -> Vec<Caption> {
    blocks
        .iter()
        .map(|b| Caption {
            id: ClipId(ctx.alloc()),
            start: b.start,
            duration: b.end - b.start,
            text: b.text.clone(),
            speaker: b.speaker.clone(),
            cue_id: None,
            settings: String::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests;
