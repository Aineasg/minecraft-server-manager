//! A comment- and order-preserving reader/writer for `server.properties`.
//!
//! `server.properties` is a Java `.properties` file. Minecraft loads it with
//! `java.util.Properties`, which means:
//!
//! * `=` **and** `:` are both valid key/value separators, with optional
//!   surrounding whitespace.
//! * the file is read as Latin-1, so any non-ASCII text (a `§` colour code in
//!   the MOTD, an accented player-facing string) is stored as a `\uXXXX` escape.
//! * `\`, `=`, `:` and leading spaces inside a value are backslash-escaped.
//! * lines starting with `#` or `!` are comments.
//!
//! Naively parsing into a map and re-serialising would reorder keys, drop the
//! explanatory comments Minecraft writes, and mangle escapes. Instead we keep the
//! file as an ordered list of lines and rewrite **only** the value span of the
//! keys the user actually changed. Every untouched line is reproduced verbatim.
//!
//! Not supported (and not used by `server.properties`): backslash line
//! continuations. Each key/value pair must be on a single line.

/// A parsed `server.properties` file that remembers its exact original layout.
#[derive(Debug, Clone)]
pub struct Properties {
    lines: Vec<Line>,
    /// Whether the original text ended with a newline.
    final_newline: bool,
    /// EOL style to use for newly appended entries (`true` = `\r\n`).
    crlf_default: bool,
}

#[derive(Debug, Clone)]
struct Line {
    kind: LineKind,
    /// This line ended with `\r\n` rather than `\n`.
    crlf: bool,
}

#[derive(Debug, Clone)]
enum LineKind {
    /// A comment, a blank line, or anything we chose not to treat as `key=value`.
    /// Stored without its trailing newline and reproduced exactly.
    Verbatim(String),
    Entry(Entry),
}

#[derive(Debug, Clone)]
struct Entry {
    /// Leading whitespace before the key (`server.properties` never uses any,
    /// but we round-trip it anyway).
    indent: String,
    /// The key exactly as written on disk (still escaped).
    raw_key: String,
    /// The decoded key, used for lookups.
    key: String,
    /// Everything between the key and the value: whitespace, the `=`/`:`, more
    /// whitespace. Preserved so `spawn-protection = 16` keeps its spaces.
    sep: String,
    /// The value exactly as written on disk (still escaped).
    raw_value: String,
    /// The decoded value.
    value: String,
}

impl Properties {
    /// Parse `server.properties` text. Never fails: anything we can't interpret
    /// as a key/value pair is kept as a verbatim line.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let final_newline = text.is_empty() || text.ends_with('\n');
        let mut crlf_votes: i32 = 0;
        let mut lines = Vec::new();

        for raw in split_lines(text) {
            let (body, crlf) = match raw.strip_suffix('\r') {
                Some(b) => (b, true),
                None => (raw, false),
            };
            crlf_votes += if crlf { 1 } else { -1 };

            let indent_len = body.len() - body.trim_start_matches(WS).len();
            let (indent, content) = body.split_at(indent_len);
            let is_comment =
                content.is_empty() || content.starts_with('#') || content.starts_with('!');

            let kind = if is_comment {
                LineKind::Verbatim(body.to_string())
            } else if let Some((raw_key, sep, raw_value)) = split_kv(content) {
                LineKind::Entry(Entry {
                    indent: indent.to_string(),
                    key: unescape(&raw_key),
                    raw_key,
                    sep,
                    value: unescape(&raw_value),
                    raw_value,
                })
            } else {
                LineKind::Verbatim(body.to_string())
            };
            lines.push(Line { kind, crlf });
        }

        Self {
            lines,
            final_newline,
            crlf_default: crlf_votes > 0,
        }
    }

    /// Serialise back to text, reproducing every untouched line byte-for-byte.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        let last = self.lines.len().saturating_sub(1);
        for (idx, line) in self.lines.iter().enumerate() {
            match &line.kind {
                LineKind::Verbatim(s) => out.push_str(s),
                LineKind::Entry(e) => {
                    out.push_str(&e.indent);
                    out.push_str(&e.raw_key);
                    out.push_str(&e.sep);
                    out.push_str(&e.raw_value);
                }
            }
            if idx != last || self.final_newline {
                if line.crlf {
                    out.push('\r');
                }
                out.push('\n');
            }
        }
        out
    }

    /// The decoded value for `key`, if present.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entry(key).map(|e| e.value.as_str())
    }

    /// Parse the value of `key` as a boolean (`true`/`false`, case-insensitive).
    #[must_use]
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        match self.get(key)?.trim().to_ascii_lowercase().as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        }
    }

    /// Parse the value of `key` as a signed integer.
    #[must_use]
    pub fn get_i64(&self, key: &str) -> Option<i64> {
        self.get(key)?.trim().parse().ok()
    }

    #[must_use]
    pub fn contains_key(&self, key: &str) -> bool {
        self.entry(key).is_some()
    }

    /// Set `key` to `value`, re-escaping as Java's `Properties.store` would.
    ///
    /// An existing key keeps its position, separator style and surrounding
    /// comments; only the value span changes. A new key is appended.
    pub fn set(&mut self, key: &str, value: &str) {
        for line in &mut self.lines {
            if let LineKind::Entry(e) = &mut line.kind {
                if e.key == key {
                    e.value = value.to_string();
                    e.raw_value = escape(value, false);
                    return;
                }
            }
        }
        self.lines.push(Line {
            kind: LineKind::Entry(Entry {
                indent: String::new(),
                raw_key: escape(key, true),
                key: key.to_string(),
                sep: "=".to_string(),
                raw_value: escape(value, false),
                value: value.to_string(),
            }),
            crlf: self.crlf_default,
        });
        // Guarantee the appended line is on its own row even if the file had no
        // trailing newline before.
        self.final_newline = true;
    }

    /// Convenience for boolean values.
    pub fn set_bool(&mut self, key: &str, value: bool) {
        self.set(key, if value { "true" } else { "false" });
    }

    /// Remove `key` entirely (its line disappears). Returns whether it existed.
    pub fn remove(&mut self, key: &str) -> bool {
        let before = self.lines.len();
        self.lines.retain(|line| !matches!(&line.kind, LineKind::Entry(e) if e.key == key));
        self.lines.len() != before
    }

    /// Iterate over `(key, value)` pairs in file order, decoded.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.lines.iter().filter_map(|line| match &line.kind {
            LineKind::Entry(e) => Some((e.key.as_str(), e.value.as_str())),
            LineKind::Verbatim(_) => None,
        })
    }

    fn entry(&self, key: &str) -> Option<&Entry> {
        self.lines.iter().find_map(|line| match &line.kind {
            LineKind::Entry(e) if e.key == key => Some(e),
            _ => None,
        })
    }
}

impl Default for Properties {
    fn default() -> Self {
        Self {
            lines: Vec::new(),
            final_newline: true,
            crlf_default: false,
        }
    }
}

const WS: [char; 3] = [' ', '\t', '\u{0C}'];

/// Split text into lines without trailing `\n`. A trailing newline does not
/// yield a spurious empty final line.
fn split_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut v: Vec<&str> = text.split('\n').collect();
    if text.ends_with('\n') {
        v.pop();
    }
    v
}

/// Split a non-comment line (leading whitespace already removed) into
/// `(raw_key, separator, raw_value)`. Returns `None` only for an empty key.
fn split_kv(s: &str) -> Option<(String, String, String)> {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;

    // Key: everything up to the first unescaped separator char or whitespace.
    let mut raw_key = String::new();
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            raw_key.push(c);
            if let Some(&next) = chars.get(i + 1) {
                raw_key.push(next);
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if c == '=' || c == ':' || WS.contains(&c) {
            break;
        }
        raw_key.push(c);
        i += 1;
    }
    if raw_key.is_empty() {
        return None;
    }

    // Separator: optional whitespace, then optionally one `=` or `:`, then
    // optional whitespace.
    let sep_start = i;
    while i < chars.len() && WS.contains(&chars[i]) {
        i += 1;
    }
    if i < chars.len() && (chars[i] == '=' || chars[i] == ':') {
        i += 1;
        while i < chars.len() && WS.contains(&chars[i]) {
            i += 1;
        }
    }
    let sep: String = chars[sep_start..i].iter().collect();
    let raw_value: String = chars[i..].iter().collect();
    Some((raw_key, sep, raw_value))
}

/// Decode Java `.properties` escapes.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('f') => out.push('\u{0C}'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                if let Ok(cp) = u32::from_str_radix(&hex, 16) {
                    if let Some(ch) = char::from_u32(cp) {
                        out.push(ch);
                    }
                }
            }
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// Encode a string the way `java.util.Properties#store` does.
///
/// `escape_all_spaces` is true for keys (every space is escaped) and false for
/// values (only a leading space needs escaping).
fn escape(s: &str, escape_all_spaces: bool) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for (idx, c) in s.chars().enumerate() {
        match c {
            ' ' => {
                if escape_all_spaces || idx == 0 {
                    out.push('\\');
                }
                out.push(' ');
            }
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{0C}' => out.push_str("\\f"),
            '=' => out.push_str("\\="),
            ':' => out.push_str("\\:"),
            // `#` and `!` are only significant at the start of a line; Java does
            // not escape them inside values, and neither do we in keys because
            // `server.properties` keys never contain them.
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
#Minecraft server properties
#Mon Aug 25 12:00:00 EEST 2026
motd=A Minecraft Server
spawn-protection = 16
max-players:20
online-mode=true
";

    #[test]
    fn round_trips_untouched_file_byte_for_byte() {
        let p = Properties::parse(SAMPLE);
        assert_eq!(p.render(), SAMPLE);
    }

    #[test]
    fn reads_both_separators_and_spacing() {
        let p = Properties::parse(SAMPLE);
        assert_eq!(p.get("motd"), Some("A Minecraft Server"));
        assert_eq!(p.get_i64("spawn-protection"), Some(16));
        assert_eq!(p.get_i64("max-players"), Some(20));
        assert_eq!(p.get_bool("online-mode"), Some(true));
    }

    #[test]
    fn set_existing_key_changes_only_that_value() {
        let mut p = Properties::parse(SAMPLE);
        p.set("online-mode", "false");
        let expected = SAMPLE.replace("online-mode=true", "online-mode=false");
        assert_eq!(p.render(), expected);
    }

    #[test]
    fn set_preserves_separator_and_whitespace_style() {
        let mut p = Properties::parse(SAMPLE);
        p.set("spawn-protection", "0");
        assert!(p.render().contains("spawn-protection = 0"));
        p.set("max-players", "8");
        assert!(p.render().contains("max-players:8"));
    }

    #[test]
    fn set_new_key_is_appended() {
        let mut p = Properties::parse(SAMPLE);
        p.set("level-seed", "12345");
        assert_eq!(p.render(), format!("{SAMPLE}level-seed=12345\n"));
    }

    #[test]
    fn non_ascii_values_are_stored_as_unicode_escapes() {
        let mut p = Properties::parse("motd=hi\n");
        p.set("motd", "\u{a7}6Golden\u{a7}r Server");
        let text = p.render();
        assert!(text.contains("\\u00a7"), "got: {text}");
        assert!(!text.contains('\u{a7}'));
        // ...and it decodes back to the original.
        let reparsed = Properties::parse(&text);
        assert_eq!(reparsed.get("motd"), Some("\u{a7}6Golden\u{a7}r Server"));
    }

    #[test]
    fn colons_in_values_are_escaped_and_unescaped() {
        let p = Properties::parse("resource-pack=https\\://example.com/pack.zip\n");
        assert_eq!(p.get("resource-pack"), Some("https://example.com/pack.zip"));

        let mut p2 = Properties::parse("resource-pack=\n");
        p2.set("resource-pack", "https://example.com/pack.zip");
        assert_eq!(p2.render(), "resource-pack=https\\://example.com/pack.zip\n");
    }

    #[test]
    fn crlf_file_round_trips() {
        let text = "a=1\r\nb=2\r\n";
        let mut p = Properties::parse(text);
        assert_eq!(p.render(), text);
        p.set("c", "3");
        assert_eq!(p.render(), "a=1\r\nb=2\r\nc=3\r\n");
    }

    #[test]
    fn missing_final_newline_is_preserved_until_a_key_is_appended() {
        let p = Properties::parse("a=1\nb=2");
        assert_eq!(p.render(), "a=1\nb=2");

        let mut p2 = Properties::parse("a=1\nb=2");
        p2.set("c", "3");
        assert_eq!(p2.render(), "a=1\nb=2\nc=3\n");
    }

    #[test]
    fn comments_and_bang_comments_survive() {
        let text = "# a comment\n! also a comment\nkey=value\n";
        let p = Properties::parse(text);
        assert_eq!(p.render(), text);
        assert_eq!(p.get("key"), Some("value"));
    }

    #[test]
    fn remove_deletes_the_line() {
        let mut p = Properties::parse(SAMPLE);
        assert!(p.remove("motd"));
        assert!(!p.render().contains("motd"));
        assert!(!p.remove("motd"));
    }
}
