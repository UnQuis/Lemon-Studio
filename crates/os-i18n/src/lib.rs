// Copyright (C) 2026 ItsAlphaNeon. Lemon Studio: https://github.com/ItsAlphaNeon/Lemon-Studio
// SPDX-License-Identifier: GPL-3.0-or-later
//
// This code, in its entirety, is NOT to be used as part of the ArtCraft suite: not this file, not any part of it,
// and not any other part of Lemon Studio.

//! Interface translations: language bundles and the lookup the canvas paints through.
//!
//! The interface is written in English and painted through [`tr`], which replaces a whole string when the
//! active bundle has a line for it (an exact match) or one of its patterns does. Anything the bundle does
//! not know stays as it is: file names, project names and numbers are never touched.
//!
//! A bundle is a plain-text `.lang` file of `source=translation` lines:
//!
//! - `\n`, `\s` (a space), `\\` and `\=` are escapes; `#` starts a comment; the optional first line
//!   `# name: Русский` names the language (`ru.lang` names it `ru` otherwise).
//! - Later lines override earlier ones, so a user's own file on top of a bundled one only spells out what it
//!   changes.
//! - A source with `{0}`, `{1}`... is a pattern: it matches whole composite strings (`Saved {0}`) and the
//!   translation may use the captures in any order.
//!
//! Bundled languages live in `lang/` (see `ru.lang`); a `.lang` file dropped in the user folder's
//! `languages/` adds a language without a rebuild, or extends a bundled one (see the application crate).
//! `OPENSTUDIO_TEXT_DUMP=<file>` appends every drawn string once, which is how a bundle is kept complete.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Mutex, OnceLock, RwLock};

/// A language the binary carries: its code, its name, and its bundle (`None`: the interface's own English).
pub struct Bundle {
    pub code: &'static str,
    pub name: &'static str,
    pub text: Option<&'static str>,
}

/// The languages built into the binary.
pub const BUNDLES: &[Bundle] = &[
    Bundle { code: "en", name: "English (US)", text: None },
    Bundle { code: "ru", name: "Русский", text: Some(include_str!("../lang/ru.lang")) },
];

/// A language added from a user `.lang` file at runtime (its name is the file's `# name:` line or its code).
struct Added {
    code: String,
    name: String,
    text: String,
}

static ADDED: Mutex<Vec<Added>> = Mutex::new(Vec::new());

/// The dictionary in use: exact lines and patterns. Replaced whole when the language changes; the previous one is
/// leaked, so a painter's borrowed `Cow` stays valid and a language switch (a rare, manual action) costs nothing.
#[derive(Default)]
struct Dict {
    exact: HashMap<Box<str>, Box<str>>,
    patterns: Vec<Pattern>,
}

impl Dict {
    /// Whether the dictionary would change anything (the fast path when the bundle is not loaded).
    fn is_empty(&self) -> bool {
        self.exact.is_empty() && self.patterns.is_empty()
    }
}

/// A `source` line with `{0}`, `{1}`... placeholders: the literal segments a candidate must contain in order, and
/// the translation with its pieces (a capture may be used in any order, or twice).
struct Pattern {
    /// The source line's key (`Saved {0}`): a later line with the same one replaces this pattern.
    key: Box<str>,
    segs: Vec<Box<str>>,
    pieces: Vec<Piece>,
    /// How many captures the patterns makes (`segs.len() - 1`).
    caps: usize,
}

enum Piece {
    Lit(Box<str>),
    Cap(usize),
}

impl Pattern {
    /// The translation of `s` when the pattern matches it whole.
    fn apply(&self, s: &str) -> Option<String> {
        let rest = s.strip_prefix(self.segs.first()?.as_ref())?;
        // `segs` has one more entry than there are captures, so its last one is the candidate's tail.
        let mut t = rest.strip_suffix(self.segs.last()?.as_ref())?;
        let mut caps: Vec<&str> = Vec::with_capacity(self.caps);
        for seg in &self.segs[1..self.segs.len() - 1] {
            let i = t.find(seg.as_ref())?;
            caps.push(&t[..i]);
            t = &t[i + seg.len()..];
        }
        caps.push(t);
        let mut out = String::new();
        for p in &self.pieces {
            match p {
                Piece::Lit(x) => out.push_str(x),
                Piece::Cap(i) => out.push_str(caps.get(*i)?),
            }
        }
        Some(out)
    }
}

/// The dictionary pointer (null until a language is set).
static DICT: AtomicPtr<Dict> = AtomicPtr::new(std::ptr::null_mut());

/// The active language: (code, name). An empty code is the interface's own English.
static CURRENT: RwLock<(String, String)> = RwLock::new((String::new(), String::new()));

fn dict() -> &'static Dict {
    static EMPTY: OnceLock<&'static Dict> = OnceLock::new();
    let p = DICT.load(Ordering::Acquire);
    if p.is_null() {
        return EMPTY.get_or_init(|| Box::leak(Box::new(Dict::default())));
    }
    // SAFETY: the pointer is only ever set to a leaked `Dict`, and never freed.
    unsafe { &*p }
}

/// The languages the user can choose: the bundled ones, then the ones added from `.lang` files (same code: the
/// added one wins).
pub fn languages() -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = BUNDLES.iter().map(|b| (b.code.to_string(), b.name.to_string())).collect();
    for a in ADDED.lock().unwrap_or_else(|e| e.into_inner()).iter() {
        match v.iter_mut().find(|(c, _)| *c == a.code) {
            Some((_, n)) => *n = a.name.clone(),
            None => v.push((a.code.clone(), a.name.clone())),
        }
    }
    v
}

/// The active language's code (empty: the interface's own English).
pub fn current_code() -> String {
    CURRENT.read().unwrap_or_else(|e| e.into_inner()).0.clone()
}

/// The active language's display name.
pub fn current_name() -> String {
    let c = CURRENT.read().unwrap_or_else(|e| e.into_inner());
    if c.0.is_empty() { "English (US)".to_string() } else { c.1.clone() }
}

/// Makes `code` the active language: the bundle the binary carries under that code plus, when the user has one,
/// their `languages/<code>.lang` on top (added with [`add_language`]). Unknown codes: English.
pub fn set_language(code: &str) {
    let embedded = BUNDLES.iter().find(|b| b.code == code).and_then(|b| b.text);
    let added = ADDED.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|a| a.code == code).map(|a| a.text.clone());
    let mut d = Dict::default();
    if let Some(text) = embedded {
        parse(&mut d, text);
    }
    if let Some(text) = added.as_deref() {
        parse(&mut d, text);
    }
    // An unknown code is English (a bundle was removed, or the saved file outlived it).
    let known = BUNDLES.iter().any(|b| b.code == code) || added.is_some();
    if !known || code == "en" {
        *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = (String::new(), "English (US)".to_string());
    } else {
        let name = languages().into_iter().find(|(c, _)| c == code).map(|(_, n)| n).unwrap_or_else(|| code.to_string());
        *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = (code.to_string(), name);
    }
    DICT.store(Box::leak(Box::new(d)), Ordering::Release);
}

/// Adds (or replaces) a language from a user `.lang` file's text; `code` is the file stem.
pub fn add_language(code: &str, name: Option<&str>, text: &str) {
    let name = name.map(str::to_string).or_else(|| name_of(text)).unwrap_or_else(|| code.to_string());
    let added = Added { code: code.to_string(), name, text: text.to_string() };
    let mut a = ADDED.lock().unwrap_or_else(|e| e.into_inner());
    match a.iter_mut().find(|x| x.code == code) {
        Some(x) => *x = added,
        None => a.push(added),
    }
}

/// The name in a bundle's `# name: Русский` header line, when it has one.
pub fn name_of(text: &str) -> Option<String> {
    text.lines().filter(|l| l.trim_start().starts_with('#')).find_map(|l| {
        let rest = l.trim_start().trim_start_matches('#').trim_start();
        rest.strip_prefix("name:").map(|n| n.trim().to_string()).filter(|n| !n.is_empty())
    })
}

/// The translation of `s` in the active language: the whole string when the bundle knows it, else unchanged.
pub fn tr(s: &str) -> Cow<'_, str> {
    dump(s);
    let d = dict();
    if d.is_empty() || s.is_empty() {
        return Cow::Borrowed(s);
    }
    if let Some(v) = d.exact.get(s) {
        return Cow::Borrowed(v);
    }
    if let Some(v) = d.patterns.iter().find_map(|p| p.apply(s)) {
        return Cow::Owned(v);
    }
    Cow::Borrowed(s)
}

/// Parses `text` into `d`: `source=translation` lines, later lines overriding earlier ones.
fn parse(d: &mut Dict, text: &str) {
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let Some((key, value)) = split_unescaped(line, '=') else { continue };
        let key = unescape(key);
        if key.is_empty() {
            continue;
        }
        let value = unescape(value);
        match pattern_of(&key, &value) {
            // A later line with the same source replaces the earlier one in place, so a user's file overrides a
            // bundled pattern the way it overrides an exact line, and the bundle's own order still decides among
            // overlapping shapes.
            Some(p) => match d.patterns.iter_mut().find(|q| q.key == p.key) {
                Some(q) => *q = p,
                None => d.patterns.push(p),
            },
            None => {
                d.exact.insert(key.into(), value.into());
            }
        }
    }
}

/// The first `sep` in `s` that is not escaped (`\=` is a literal one).
fn split_unescaped(s: &str, sep: char) -> Option<(&str, &str)> {
    let mut esc = false;
    for (i, c) in s.char_indices() {
        if esc {
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if c == sep {
            return Some((&s[..i], &s[i + c.len_utf8()..]));
        }
    }
    None
}

/// `\n`, `\s`, `\\`, `\=` (anything else after a backslash keeps the backslash).
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('s') => out.push(' '),
            Some('\\') => out.push('\\'),
            Some('=') => out.push('='),
            Some(o) => {
                out.push('\\');
                out.push(o);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// A `{0}` placeholder in `s` at `at` (a byte index of a `{`): the capture index and the placeholder's length.
fn placeholder(s: &str, at: usize) -> Option<(usize, usize)> {
    let rest = &s[at + 1..];
    let end = rest.find('}')?;
    let inside = &rest[..end];
    if inside.len() != 1 || !inside.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    inside.parse().ok().map(|i| (i, end + 2))
}

/// `Pattern` when the source has placeholders; `None` = a plain line. `key` and `value` are already unescaped.
fn pattern_of(key: &str, value: &str) -> Option<Pattern> {
    let mut segs: Vec<Box<str>> = Vec::new();
    let mut s = String::new();
    let mut i = 0;
    let mut caps = 0;
    while i < key.len() {
        if key.as_bytes()[i] == b'{'
            && let Some((idx, len)) = placeholder(key, i)
        {
            segs.push(std::mem::take(&mut s).into());
            caps = caps.max(idx + 1);
            i += len;
        } else {
            let c = key[i..].chars().next()?;
            s.push(c);
            i += c.len_utf8();
        }
    }
    if caps == 0 {
        return None;
    }
    segs.push(s.into());
    let mut pieces: Vec<Piece> = Vec::new();
    let mut s = String::new();
    let mut i = 0;
    while i < value.len() {
        if value.as_bytes()[i] == b'{'
            && let Some((idx, len)) = placeholder(value, i)
        {
            if !s.is_empty() {
                pieces.push(Piece::Lit(std::mem::take(&mut s).into()));
            }
            pieces.push(Piece::Cap(idx));
            i += len;
        } else {
            let c = value[i..].chars().next()?;
            s.push(c);
            i += c.len_utf8();
        }
    }
    if !s.is_empty() {
        pieces.push(Piece::Lit(s.into()));
    }
    Some(Pattern { key: key.into(), segs, pieces, caps })
}

/// `OPENSTUDIO_TEXT_DUMP=<file>`: appends every drawn string once (how a translator keeps a bundle complete).
fn dump(s: &str) {
    static DUMP: OnceLock<Option<Mutex<Dump>>> = OnceLock::new();
    let Some(d) = DUMP.get_or_init(Dump::open).as_ref() else { return };
    let Ok(mut d) = d.lock() else { return };
    if s.is_empty() || !d.seen.insert(s.to_string()) {
        return;
    }
    use std::io::Write;
    let line = s.replace('\\', "\\\\").replace('\n', "\\n");
    let _ = writeln!(d.file, "{line}");
    let _ = d.file.flush();
}

struct Dump {
    file: std::fs::File,
    seen: HashSet<String>,
}

impl Dump {
    fn open() -> Option<Mutex<Dump>> {
        let path = std::env::var_os("OPENSTUDIO_TEXT_DUMP").map(PathBuf::from)?;
        let file = std::fs::OpenOptions::new().create(true).write(true).truncate(true).open(path).ok()?;
        Some(Mutex::new(Dump { file, seen: HashSet::new() }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The registry and the active language are process-global: the tests that touch them run one at a time.
    static LOCK: Mutex<()> = Mutex::new(());

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// What the bundle `code` says about `key`, for tests that must not hard-code a translation.
    fn bundled(code: &str, key: &str) -> String {
        let text = BUNDLES.iter().find(|b| b.code == code).and_then(|b| b.text).unwrap_or("");
        let mut d = Dict::default();
        parse(&mut d, text);
        d.exact.get(key).map(|v| v.to_string()).unwrap_or_else(|| key.to_string())
    }

    #[test]
    fn lines_override_and_escapes_parse() {
        let mut d = Dict::default();
        parse(&mut d, "# comment\nChannel rack=Rack каналов\nChannel rack=Rack\nvolume\\s=\\svol\nmulti\\nline=one\\ntwo\nx\\=y=by\n");
        assert_eq!(d.exact.get("Channel rack").map(|v| &**v), Some("Rack"));
        assert_eq!(d.exact.get("volume ").map(|v| &**v), Some(" vol"));
        assert_eq!(d.exact.get("multi\nline").map(|v| &**v), Some("one\ntwo"));
        assert_eq!(d.exact.get("x=y").map(|v| &**v), Some("by"));
    }

    #[test]
    fn patterns_capture_and_reorder() {
        let mut d = Dict::default();
        parse(&mut d, "Saved {0}=Сохранено: {0}\n{0}: not available yet=Ещё нет: {0}\nInsert {0}, slot {1}=Вставка {1} в {0}\n");
        assert_eq!(d.patterns.len(), 3);
        let p = |s: &str| d.patterns.iter().find_map(|p| p.apply(s));
        assert_eq!(p("Saved demo.flp").as_deref(), Some("Сохранено: demo.flp"));
        assert_eq!(p("Reverb: not available yet").as_deref(), Some("Ещё нет: Reverb"));
        assert_eq!(p("Insert 3, slot 12").as_deref(), Some("Вставка 12 в 3"));
        assert_eq!(p("nothing here"), None);
    }

    #[test]
    fn languages_switch_and_unknown_text_passes_through() {
        let _g = lock();
        add_language("zz", Some("Testish"), "Channel rack=Rack zz\nSaved {0}=Sohraneno {0}\n");
        set_language("en");
        assert!(dict().is_empty());
        assert_eq!(tr("Channel rack"), "Channel rack");
        set_language("zz");
        assert_eq!(tr("Channel rack"), "Rack zz");
        assert_eq!(tr("Saved x.flp"), "Sohraneno x.flp");
        assert_eq!(tr("some project.flp"), "some project.flp");
        set_language("en");
        assert_eq!(tr("Channel rack"), "Channel rack");
    }

    #[test]
    fn a_user_bundle_on_top_of_a_bundled_one() {
        let _g = lock();
        let expected = bundled("ru", "Channel rack");
        add_language("ru", None, "");
        set_language("ru");
        assert_eq!(tr("Channel rack"), expected.as_str());
        // A user file only spells out what it changes.
        add_language("ru", None, "Channel rack=Канальный рэк\n");
        set_language("ru");
        assert_eq!(tr("Channel rack"), "Канальный рэк");
        assert_eq!(tr("some project.flp"), "some project.flp");
        // A pattern line goes the same way; overlapping shapes the file does not touch keep the bundle's order.
        add_language("ru", None, "Saved {0}=Записано {0}\n");
        set_language("ru");
        assert_eq!(tr("Saved x.flp"), "Записано x.flp");
        assert_eq!(tr("Saved x.flp (not stored yet: y.flp)"), "Сохранено x.flp (пока не записано: y.flp)");
        add_language("ru", None, "");
        set_language("en");
    }

    #[test]
    fn the_built_in_bundles_are_named_and_load() {
        let _g = lock();
        assert!(BUNDLES.iter().any(|b| b.code == "en" && b.name == "English (US)" && b.text.is_none()));
        assert!(BUNDLES.iter().any(|b| b.code == "ru" && b.name == "Русский"));
        for b in BUNDLES {
            if let Some(t) = b.text {
                let mut d = Dict::default();
                parse(&mut d, t);
                assert!(!d.exact.is_empty() || !d.patterns.is_empty(), "bundle {} is empty", b.code);
            }
        }
    }

    #[test]
    fn a_user_file_adds_a_language_of_its_own() {
        let _g = lock();
        add_language("yy", None, "# name: Testish\nChannel rack=Something else\n");
        assert!(languages().iter().any(|(c, n)| c == "yy" && n == "Testish"));
        // Unknown code: the interface's own English.
        set_language("qq");
        assert_eq!(tr("Channel rack"), "Channel rack");
        assert_eq!(current_name(), "English (US)");
        set_language("yy");
        assert_eq!(current_code(), "yy");
        assert_eq!(current_name(), "Testish");
        assert_eq!(tr("Channel rack"), "Something else");
        add_language("yy", None, "");
        set_language("en");
    }
}
