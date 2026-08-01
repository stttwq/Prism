//! Versioned pinyin encoding and matching shared by broker and indexer.

use ::pinyin::ToPinyin;

pub const PINYIN_DICTIONARY_VERSION: &str = "pinyin-0.10.0/pinyin-data-0.13.0+prism-phrases-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PinyinMatchKind {
    Full,
    Initials,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinyinMatch {
    pub kind: PinyinMatchKind,
    pub class: u8,
    pub position: u32,
    pub score: u32,
    pub spans: Vec<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Token {
    reading: String,
    utf16_start: u16,
    utf16_len: u16,
}

// Longest-match phrase rules. Changing this table requires a dictionary-version bump.
const PHRASES: &[(&str, &[&str])] = &[
    ("重庆", &["chong", "qing"]),
    ("重慶", &["chong", "qing"]),
    ("音乐", &["yin", "yue"]),
    ("音樂", &["yin", "yue"]),
    ("银行", &["yin", "hang"]),
    ("銀行", &["yin", "hang"]),
    ("行长", &["hang", "zhang"]),
    ("行長", &["hang", "zhang"]),
    ("长安", &["chang", "an"]),
    ("長安", &["chang", "an"]),
    ("厦门", &["xia", "men"]),
    ("廈門", &["xia", "men"]),
    ("朝阳", &["chao", "yang"]),
    ("朝陽", &["chao", "yang"]),
    ("快乐", &["kuai", "le"]),
    ("快樂", &["kuai", "le"]),
    ("角色", &["jue", "se"]),
    ("便宜", &["pian", "yi"]),
];

pub fn normalize_query(query: &str) -> Option<String> {
    let mut normalized = String::with_capacity(query.len());
    let mut latin_letters = 0usize;
    for ch in query.chars() {
        match ch {
            ' ' | '\t' | '\r' | '\n' | '\'' | '\u{2019}' => {}
            'ü' | 'Ü' => {
                normalized.push('v');
                latin_letters += 1;
            }
            ':' if normalized.ends_with('u') => {
                normalized.pop();
                normalized.push('v');
            }
            ch if ch.is_ascii_alphanumeric() => {
                normalized.push(ch.to_ascii_lowercase());
                if ch.is_ascii_alphabetic() {
                    latin_letters += 1;
                }
            }
            _ => return None,
        }
    }
    (latin_letters >= 2 && !normalized.is_empty()).then_some(normalized)
}

pub fn match_name(name: &str, query: &str) -> Option<PinyinMatch> {
    let query = normalize_query(query)?;
    let (tokens, has_han) = encode_tokens(name);
    if !has_han {
        return None;
    }
    match_tokens(&tokens, query.as_bytes(), PinyinMatchKind::Full)
        .or_else(|| match_tokens(&tokens, query.as_bytes(), PinyinMatchKind::Initials))
}

pub(crate) fn encode_compact(name: &str) -> Option<Vec<u8>> {
    let (tokens, has_han) = encode_tokens(name);
    if !has_han {
        return None;
    }
    let mut bytes = Vec::new();
    for token in tokens {
        let len = u8::try_from(token.reading.len()).ok()?;
        bytes.push(len);
        bytes.extend_from_slice(&token.utf16_start.to_le_bytes());
        bytes.extend_from_slice(&token.utf16_len.to_le_bytes());
        bytes.extend_from_slice(token.reading.as_bytes());
    }
    Some(bytes)
}

#[cfg(test)]
pub(crate) fn match_compact(bytes: &[u8], query: &str) -> Option<PinyinMatch> {
    let query = normalize_query(query)?;
    match_compact_normalized(bytes, query.as_bytes())
}

pub(crate) fn match_compact_normalized(bytes: &[u8], query: &[u8]) -> Option<PinyinMatch> {
    match_compact_kind(bytes, query, PinyinMatchKind::Full)
        .or_else(|| match_compact_kind(bytes, query, PinyinMatchKind::Initials))
}

fn encode_tokens(name: &str) -> (Vec<Token>, bool) {
    let chars: Vec<(usize, char)> = name.char_indices().collect();
    let mut tokens = Vec::with_capacity(chars.len());
    let mut index = 0usize;
    let mut utf16_start = 0u16;
    let mut has_han = false;
    while index < chars.len() {
        let remaining = &name[chars[index].0..];
        if let Some((phrase, readings)) = PHRASES
            .iter()
            .filter(|(phrase, _)| remaining.starts_with(*phrase))
            .max_by_key(|(phrase, _)| phrase.chars().count())
        {
            has_han = true;
            for (offset, reading) in readings.iter().enumerate() {
                let ch = chars[index + offset].1;
                let len = ch.len_utf16() as u16;
                tokens.push(Token {
                    reading: (*reading).to_owned(),
                    utf16_start,
                    utf16_len: len,
                });
                utf16_start = utf16_start.saturating_add(len);
            }
            index += phrase.chars().count();
            continue;
        }

        let ch = chars[index].1;
        let len = ch.len_utf16() as u16;
        if let Some(reading) = ch.to_pinyin() {
            has_han = true;
            tokens.push(Token {
                reading: normalize_reading(reading.plain()),
                utf16_start,
                utf16_len: len,
            });
        } else if ch.is_ascii_alphanumeric() {
            tokens.push(Token {
                reading: ch.to_ascii_lowercase().to_string(),
                utf16_start,
                utf16_len: len,
            });
        }
        utf16_start = utf16_start.saturating_add(len);
        index += 1;
    }
    (tokens, has_han)
}

fn normalize_reading(reading: &str) -> String {
    reading
        .chars()
        .map(|ch| match ch {
            'ü' | 'Ü' => 'v',
            _ => ch.to_ascii_lowercase(),
        })
        .collect()
}

#[derive(Clone, Copy)]
struct CompactToken<'a> {
    reading: &'a [u8],
    utf16_start: u16,
    utf16_len: u16,
    next: usize,
}

fn compact_token(bytes: &[u8], cursor: usize) -> Option<CompactToken<'_>> {
    let len = *bytes.get(cursor)? as usize;
    if len == 0 {
        return None;
    }
    let utf16_start = u16::from_le_bytes([*bytes.get(cursor + 1)?, *bytes.get(cursor + 2)?]);
    let utf16_len = u16::from_le_bytes([*bytes.get(cursor + 3)?, *bytes.get(cursor + 4)?]);
    let reading_start = cursor + 5;
    let next = reading_start.checked_add(len)?;
    let reading = bytes.get(reading_start..next)?;
    (reading.is_ascii() && utf16_len > 0).then_some(CompactToken {
        reading,
        utf16_start,
        utf16_len,
        next,
    })
}

fn match_compact_kind(bytes: &[u8], query: &[u8], kind: PinyinMatchKind) -> Option<PinyinMatch> {
    let mut start_cursor = 0usize;
    let mut start_index = 0u32;
    while start_cursor < bytes.len() {
        let mut cursor = start_cursor;
        let mut query_at = 0usize;
        while cursor < bytes.len() {
            let token = compact_token(bytes, cursor)?;
            let reading = match kind {
                PinyinMatchKind::Full => token.reading,
                PinyinMatchKind::Initials => &token.reading[..1],
            };
            let remaining = &query[query_at..];
            let compared = remaining.len().min(reading.len());
            if remaining[..compared] != reading[..compared] {
                break;
            }
            query_at += compared;
            cursor = token.next;
            if query_at == query.len() {
                let at_end = cursor == bytes.len() && compared == reading.len();
                return Some(PinyinMatch {
                    kind,
                    class: if start_cursor == 0 && at_end {
                        0
                    } else if start_cursor == 0 {
                        1
                    } else {
                        2
                    },
                    position: start_index,
                    score: compact_token_count(bytes)?,
                    spans: compact_spans(bytes, start_cursor, cursor)?,
                });
            }
            if compared != reading.len() {
                break;
            }
        }
        let token = compact_token(bytes, start_cursor)?;
        start_cursor = token.next;
        start_index = start_index.saturating_add(1);
    }
    None
}

fn compact_token_count(bytes: &[u8]) -> Option<u32> {
    let mut cursor = 0usize;
    let mut count = 0u32;
    while cursor < bytes.len() {
        cursor = compact_token(bytes, cursor)?.next;
        count = count.saturating_add(1);
    }
    Some(count)
}

fn compact_spans(bytes: &[u8], start: usize, end: usize) -> Option<Vec<i32>> {
    let mut cursor = start;
    let mut spans = Vec::new();
    while cursor < end {
        let token = compact_token(bytes, cursor)?;
        let span_start = i32::from(token.utf16_start);
        let span_len = i32::from(token.utf16_len);
        if spans.len() >= 2 {
            let last = spans.len() - 2;
            if spans[last] + spans[last + 1] == span_start {
                spans[last + 1] += span_len;
                cursor = token.next;
                continue;
            }
        }
        spans.push(span_start);
        spans.push(span_len);
        cursor = token.next;
    }
    (cursor == end).then_some(spans)
}

fn match_tokens(tokens: &[Token], query: &[u8], kind: PinyinMatchKind) -> Option<PinyinMatch> {
    for start in 0..tokens.len() {
        let mut query_at = 0usize;
        let mut end = start;
        for token in &tokens[start..] {
            let reading = match kind {
                PinyinMatchKind::Full => token.reading.as_bytes(),
                PinyinMatchKind::Initials => &token.reading.as_bytes()[..1],
            };
            let remaining = &query[query_at..];
            let compared = remaining.len().min(reading.len());
            if remaining[..compared] != reading[..compared] {
                break;
            }
            query_at += compared;
            end += 1;
            if query_at == query.len() {
                let at_end = end == tokens.len() && compared == reading.len();
                return Some(PinyinMatch {
                    kind,
                    class: if start == 0 && at_end {
                        0
                    } else if start == 0 {
                        1
                    } else {
                        2
                    },
                    position: start as u32,
                    score: tokens.len() as u32,
                    spans: spans_for(&tokens[start..end]),
                });
            }
            if compared != reading.len() {
                break;
            }
        }
    }
    None
}

fn spans_for(tokens: &[Token]) -> Vec<i32> {
    let mut spans: Vec<i32> = Vec::new();
    for token in tokens {
        let start = i32::from(token.utf16_start);
        let len = i32::from(token.utf16_len);
        if spans.len() >= 2 {
            let last = spans.len() - 2;
            if spans[last] + spans[last + 1] == start {
                spans[last + 1] += len;
                continue;
            }
        }
        spans.push(start);
        spans.push(len);
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_query_contract() {
        let cases = [
            ("微信", "wx", Some(PinyinMatchKind::Initials)),
            ("微信", "weixin", Some(PinyinMatchKind::Full)),
            ("微信", "weix", Some(PinyinMatchKind::Full)),
            ("微信", "xin", Some(PinyinMatchKind::Full)),
            ("微信", "eix", None),
            ("微信开发", "wxkaifa", None),
            ("微信2026", "wx2026", Some(PinyinMatchKind::Initials)),
            ("微信beta", "weixinbeta", Some(PinyinMatchKind::Full)),
            ("重庆", "chongqing", Some(PinyinMatchKind::Full)),
            ("重慶", "cq", Some(PinyinMatchKind::Initials)),
            ("音乐", "yinyue", Some(PinyinMatchKind::Full)),
            ("銀行", "yinhang", Some(PinyinMatchKind::Full)),
            ("中国", "zhongguo", Some(PinyinMatchKind::Full)),
            ("軟體", "ruanti", Some(PinyinMatchKind::Full)),
            ("绿色", "lvse", Some(PinyinMatchKind::Full)),
            ("绿色", "lüse", Some(PinyinMatchKind::Full)),
        ];
        for (name, query, expected) in cases {
            assert_eq!(
                match_name(name, query).map(|value| value.kind),
                expected,
                "{name} / {query}"
            );
        }
    }

    #[test]
    fn normalization_and_utf16_highlights_are_stable() {
        assert_eq!(normalize_query("  LÜ' SE  ").as_deref(), Some("lvse"));
        assert_eq!(normalize_query("lU:se").as_deref(), Some("lvse"));
        let matched = match_name("A微信2026", "WEI XIN 2026").unwrap();
        assert_eq!(matched.kind, PinyinMatchKind::Full);
        assert_eq!(matched.spans, [1, 6]);
        let supplementary = match_name("𠀀微信", "wx").unwrap();
        assert_eq!(supplementary.spans, [2, 2]);
    }

    #[test]
    fn compact_round_trip_keeps_matching_contract() {
        let encoded = encode_compact("重庆2026").unwrap();
        let matched = match_compact(&encoded, "cq2026").unwrap();
        assert_eq!(matched.kind, PinyinMatchKind::Initials);
        assert_eq!(matched.spans, [0, 6]);
        assert!(match_compact(&encoded, "hongq").is_none());
    }

    #[test]
    fn single_latin_letter_does_not_enable_pinyin() {
        assert!(match_name("微信", "w").is_none());
        assert!(match_name("微信", "2").is_none());
    }
}
