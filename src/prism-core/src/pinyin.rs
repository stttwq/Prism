//! Versioned pinyin encoding and matching shared by broker and indexer.

use ::pinyin::ToPinyin;

pub const PINYIN_DICTIONARY_VERSION: &str = "pinyin-0.10.0/pinyin-data-0.13.0+prism-phrases-v2";

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
//
// P1-1（搜索报告2，2026-08-21）：18 → ~130 常用多音字词。收录原则：
// 1. 只收高频词（地名/称谓/软件域动词/日常词）；
// 2. 逐条人工核对读音——错词表会**制造**错误命中（比缺词更糟）；
// 3. 读音与库默认可能重合的词条是幂等的（override 结果与默认一致），
//    留作护栏不删；
// 4. plain 拼音无声调——同字母异声调的字（好/了/当/倒/处/量/兴…）不收，
//    收了也不改变行为。
// 词表变更必须 bump PINYIN_DICTIONARY_VERSION（触发一次拼音 sidecar 全量重建）。
const PHRASES: &[(&str, &[&str])] = &[
    // ── 地名 ──
    ("重庆", &["chong", "qing"]),
    ("重慶", &["chong", "qing"]),
    ("长安", &["chang", "an"]),
    ("長安", &["chang", "an"]),
    ("长沙", &["chang", "sha"]),
    ("长春", &["chang", "chun"]),
    ("长治", &["chang", "zhi"]),
    ("长城", &["chang", "cheng"]),
    ("厦门", &["xia", "men"]),
    ("廈門", &["xia", "men"]),
    ("大厦", &["da", "sha"]),
    ("朝阳", &["chao", "yang"]),
    ("朝陽", &["chao", "yang"]),
    ("番禺", &["pan", "yu"]),
    ("蚌埠", &["beng", "bu"]),
    ("六安", &["lu", "an"]),
    ("百色", &["bo", "se"]),
    ("句容", &["ju", "rong"]),
    ("莘县", &["shen", "xian"]),
    ("铅山", &["yan", "shan"]),
    ("泌阳", &["bi", "yang"]),
    // ── 音乐/银行/行 系列 ──
    ("音乐", &["yin", "yue"]),
    ("音樂", &["yin", "yue"]),
    ("银行", &["yin", "hang"]),
    ("銀行", &["yin", "hang"]),
    ("行长", &["hang", "zhang"]),
    ("行長", &["hang", "zhang"]),
    ("行业", &["hang", "ye"]),
    ("行情", &["hang", "qing"]),
    ("行家", &["hang", "jia"]),
    ("内行", &["nei", "hang"]),
    ("外行", &["wai", "hang"]),
    // ── 长：cháng（名物）与 zhǎng（称谓/生长）──
    ("长大", &["zhang", "da"]),
    ("长辈", &["zhang", "bei"]),
    ("兄长", &["xiong", "zhang"]),
    ("成长", &["cheng", "zhang"]),
    ("生长", &["sheng", "zhang"]),
    ("增长", &["zeng", "zhang"]),
    ("班长", &["ban", "zhang"]),
    ("校长", &["xiao", "zhang"]),
    ("厂长", &["chang", "zhang"]),
    ("队长", &["dui", "zhang"]),
    ("市长", &["shi", "zhang"]),
    ("局长", &["ju", "zhang"]),
    ("部长", &["bu", "zhang"]),
    ("会长", &["hui", "zhang"]),
    ("首长", &["shou", "zhang"]),
    ("董事长", &["dong", "shi", "zhang"]),
    // ── 重：chóng（重复义）──
    ("重新", &["chong", "xin"]),
    ("重复", &["chong", "fu"]),
    ("重阳", &["chong", "yang"]),
    ("重逢", &["chong", "feng"]),
    ("重叠", &["chong", "die"]),
    ("重申", &["chong", "shen"]),
    ("重播", &["chong", "bo"]),
    ("重启", &["chong", "qi"]),
    ("重装", &["chong", "zhuang"]),
    ("重置", &["chong", "zhi"]),
    // ── 弹：tán（动作）/dàn（弹丸）──
    ("弹窗", &["tan", "chuang"]),
    ("弹幕", &["tan", "mu"]),
    ("弹簧", &["tan", "huang"]),
    ("弹性", &["tan", "xing"]),
    ("子弹", &["zi", "dan"]),
    ("弹药", &["dan", "yao"]),
    ("导弹", &["dao", "dan"]),
    ("弹道", &["dan", "dao"]),
    // ── 调：tiáo（调节）/diào（调用）──
    ("调整", &["tiao", "zheng"]),
    ("调节", &["tiao", "jie"]),
    ("调试", &["tiao", "shi"]),
    ("调用", &["diao", "yong"]),
    ("调查", &["diao", "cha"]),
    ("调研", &["diao", "yan"]),
    ("调度", &["diao", "du"]),
    ("声调", &["sheng", "diao"]),
    // ── 觉/角 ──
    ("角色", &["jue", "se"]),
    ("主角", &["zhu", "jue"]),
    ("配角", &["pei", "jue"]),
    ("睡觉", &["shui", "jiao"]),
    ("觉得", &["jue", "de"]),
    ("直觉", &["zhi", "jue"]),
    // ── 着/血/壳/模 ──
    ("着急", &["zhao", "ji"]),
    ("着重", &["zhuo", "zhong"]),
    ("执着", &["zhi", "zhuo"]),
    ("穿着", &["chuan", "zhuo"]),
    ("血压", &["xue", "ya"]),
    ("血液", &["xue", "ye"]),
    ("血管", &["xue", "guan"]),
    ("流血", &["liu", "xue"]),
    ("输血", &["shu", "xue"]),
    ("外壳", &["wai", "ke"]),
    ("贝壳", &["bei", "ke"]),
    ("蛋壳", &["dan", "ke"]),
    ("地壳", &["di", "qiao"]),
    ("模样", &["mu", "yang"]),
    ("模板", &["mu", "ban"]),
    // ── 还：huán（归还）/hái（仍然，护栏）──
    ("还是", &["hai", "shi"]),
    ("还有", &["hai", "you"]),
    ("还原", &["huan", "yuan"]),
    ("归还", &["gui", "huan"]),
    ("偿还", &["chang", "huan"]),
    // ── 藏/曾/查/参/差 ──
    ("西藏", &["xi", "zang"]),
    ("藏族", &["zang", "zu"]),
    ("宝藏", &["bao", "zang"]),
    ("隐藏", &["yin", "cang"]),
    ("收藏", &["shou", "cang"]),
    ("曾经", &["ceng", "jing"]),
    ("曾祖", &["zeng", "zu"]),
    ("检查", &["jian", "cha"]),
    ("查找", &["cha", "zhao"]),
    ("参加", &["can", "jia"]),
    ("参考", &["can", "kao"]),
    ("参观", &["can", "guan"]),
    ("参谋", &["can", "mou"]),
    ("人参", &["ren", "shen"]),
    ("参差", &["cen", "ci"]),
    ("出差", &["chu", "chai"]),
    ("差别", &["cha", "bie"]),
    ("误差", &["wu", "cha"]),
    // ── 都：dū（都市）/dōu（副词）──
    ("都市", &["du", "shi"]),
    ("首都", &["shou", "du"]),
    ("都是", &["dou", "shi"]),
    ("全都", &["quan", "dou"]),
    // ── 传/称/畜/圈/泊/吓/塞/其他 ──
    ("传记", &["zhuan", "ji"]),
    ("自传", &["zhi", "zhuan"]),
    ("水浒传", &["shui", "hu", "zhuan"]),
    ("传说", &["chuan", "shuo"]),
    ("传输", &["chuan", "shu"]),
    ("称职", &["chen", "zhi"]),
    ("称心", &["chen", "xin"]),
    ("名称", &["ming", "cheng"]),
    ("称号", &["cheng", "hao"]),
    ("畜生", &["chu", "sheng"]),
    ("畜牧", &["xu", "mu"]),
    ("羊圈", &["yang", "juan"]),
    ("圈子", &["quan", "zi"]),
    ("湖泊", &["hu", "po"]),
    ("停泊", &["ting", "bo"]),
    ("吓一跳", &["xia", "yi", "tiao"]),
    ("恐吓", &["kong", "he"]),
    ("堵塞", &["du", "se"]),
    ("塞子", &["sai", "zi"]),
    ("大夫", &["dai", "fu"]),
    ("便宜", &["pian", "yi"]),
    ("便利", &["bian", "li"]),
    ("倔强", &["jue", "jiang"]),
    ("提防", &["di", "fang"]),
    ("揣度", &["chuai", "duo"]),
    ("伺候", &["ci", "hou"]),
    ("积攒", &["ji", "zan"]),
    ("囤积", &["tun", "ji"]),
    ("剥削", &["bo", "xue"]),
    ("殷红", &["yan", "hong"]),
    ("爪牙", &["zhao", "ya"]),
    ("咬文嚼字", &["yao", "wen", "jiao", "zi"]),
    ("纤维", &["xian", "wei"]),
    ("系统", &["xi", "tong"]),
    ("联系", &["lian", "xi"]),
    ("朝着", &["chao", "zhe"]),
    ("折腾", &["zhe", "teng"]),
    ("快乐", &["kuai", "le"]),
    ("快樂", &["kuai", "le"]),
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
        .or_else(|| match_tokens_mixed(&tokens, query.as_bytes()))
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
        // S3（PRISM-IMPL-PLAN-4-2026-08-20）：前两条纯策略都失败才试混用。
        .or_else(|| match_compact_mixed(bytes, query))
}

/// S3（PRISM-IMPL-PLAN-4-2026-08-20）：全拼与首字母逐字混用匹配（位掩码 DP）。
/// 语义：从起点 token 开始的**连续** token 段，每个 token 三选一消费查询——
/// 首字母（前进 1）/ 全拼（前进 reading.len()）/ 尾部部分（reading 以剩余查询
/// 为前缀，终态）。查询耗尽即命中。掩码 bit p = 「已消费 p 字节查询」可达；
/// 掩码归零立即换下一起点（绝大多数不命中条目在首个 token 就归零，成本与
/// 两条纯路径同量级）。命中上报 `Initials` 档（最弱拼音档）——
/// `ponytail:` 不新增 MatchKind::MixedPinyin：省掉 serde 线格式新取值 +
/// INDEXER_PROTOCOL bump + 新旧混装反序列化失败面，代价是 wxin 与 wx 同档
/// 排序。若实测出现「混用命中被全拼命中不合理压制」的案例，再插入
/// MixedPinyin 变体（声明序在 FullPinyin 与 Initials 之间）并 bump 协议版本。
/// 查询 > 63 字节跳过混用（u64 位掩码上限），退回两条纯路径。
fn match_compact_mixed(bytes: &[u8], query: &[u8]) -> Option<PinyinMatch> {
    if query.is_empty() || query.len() > 63 {
        return None;
    }
    let done_bit = 1u64 << query.len();
    let mut start_cursor = 0usize;
    let mut start_index = 0u32;
    while start_cursor < bytes.len() {
        let mut mask = 1u64;
        let mut cursor = start_cursor;
        while cursor < bytes.len() && mask != 0 {
            let token = compact_token(bytes, cursor)?;
            let mut next = 0u64;
            let mut bits = mask;
            while bits != 0 {
                let p = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                // 终态：reading 以剩余查询为前缀（尾部部分匹配，允许名字尾部未覆盖）。
                if token.reading.starts_with(&query[p..]) {
                    next |= done_bit;
                }
                if token.reading[0] == query[p] {
                    next |= 1u64 << (p + 1);
                }
                if query[p..].starts_with(token.reading) {
                    next |= 1u64 << (p + token.reading.len());
                }
            }
            if next & done_bit != 0 {
                let end_cursor = token.next;
                // class 0 只在「起点为 0 + 覆盖到名字末尾 + 最后消费是全拼」时给，
                // 与两条纯路径的 at_end 语义对齐（前一条状态含对应全拼位）。
                let full_at_end = end_cursor == bytes.len()
                    && query.len() >= token.reading.len()
                    && mask & (1u64 << (query.len() - token.reading.len())) != 0;
                return Some(PinyinMatch {
                    kind: PinyinMatchKind::Initials,
                    class: if start_cursor == 0 && full_at_end {
                        0
                    } else if start_cursor == 0 {
                        1
                    } else {
                        2
                    },
                    position: start_index,
                    score: compact_token_count(bytes)?,
                    spans: compact_spans(bytes, start_cursor, end_cursor)?,
                });
            }
            mask = next;
            cursor = token.next;
        }
        let token = compact_token(bytes, start_cursor)?;
        start_cursor = token.next;
        start_index = start_index.saturating_add(1);
    }
    None
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

/// S3（PRISM-IMPL-PLAN-4-2026-08-20）：即时匹配路径（apps / 窗口 / 历史候选）
/// 的混用匹配。与 `match_compact_mixed` 保持判定一致——
/// apps::precomputed_pinyin_matches_the_on_the_fly_encoder 锚定两条路径等价。
fn match_tokens_mixed(tokens: &[Token], query: &[u8]) -> Option<PinyinMatch> {
    if query.is_empty() || query.len() > 63 {
        return None;
    }
    let done_bit = 1u64 << query.len();
    for start in 0..tokens.len() {
        let mut mask = 1u64;
        for (offset, token) in tokens[start..].iter().enumerate() {
            let reading = token.reading.as_bytes();
            let mut next = 0u64;
            let mut bits = mask;
            while bits != 0 {
                let p = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                if reading.starts_with(&query[p..]) {
                    next |= done_bit;
                }
                if reading[0] == query[p] {
                    next |= 1u64 << (p + 1);
                }
                if query[p..].starts_with(reading) {
                    next |= 1u64 << (p + reading.len());
                }
            }
            if next & done_bit != 0 {
                let end = start + offset + 1;
                let full_at_end = end == tokens.len()
                    && query.len() >= reading.len()
                    && mask & (1u64 << (query.len() - reading.len())) != 0;
                return Some(PinyinMatch {
                    kind: PinyinMatchKind::Initials,
                    class: if start == 0 && full_at_end {
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
            mask = next;
            if mask == 0 {
                break;
            }
        }
    }
    None
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
            // S3 前置调查结论（PRISM-IMPL-PLAN-4 §1.4）：「微信开发/wxkaifa」在
            // 混用语义下是合法输入（wx 首字母 + kaifa 全拼，连续 token 段）——
            // 从 None 显式翻转为 Some，这就是用户会打出的「混用」定义。
            // 若要收紧（限制首字母段与全拼段的交替次数），必须重新审视语义。
            ("微信开发", "wxkaifa", Some(PinyinMatchKind::Initials)),
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
            // S3：全拼与首字母逐字混用（w 首字母 + xin 全拼；wang+yi 全拼 + y+y 首字母）。
            ("微信", "wxin", Some(PinyinMatchKind::Initials)),
            ("网易云音乐", "wangyiyy", Some(PinyinMatchKind::Initials)),
            // S3 负例（防召回过宽）：tenxunhuiyi 缺 g（teng 的尾部部分只允许在
            // 查询耗尽处发生，teng 之后查询还有内容，不命中是对的）。
            ("腾讯会议", "tenxunhuiyi", None),
            // S3 前置调查（PRISM-IMPL-PLAN-4 §1.4）：dysp 对「抖音短视频」需要跳过
            // 第 3 音节（短/duan 的首字母 d 不在查询里）——那是子序列匹配
            //（fzf 式），方案 §6.2 明确不做，保持 None。若用户确有「抖音视频」
            //（dou yin shi pin）命名，dysp 经混用/首字母路径正常命中。
            ("抖音短视频", "dysp", None),
        ];
        for (name, query, expected) in cases {
            assert_eq!(
                match_name(name, query).map(|value| value.kind),
                expected,
                "{name} / {query}"
            );
        }
    }

    /// S3：混用命中的 class / position / spans 细节——连续 token 段、
    /// 尾部允许未覆盖、起点决定 class 档位。
    #[test]
    fn s3_mixed_match_metadata_and_spans() {
        // w 首字母 + xin 全拼：起点 0、覆盖到末尾 → class 0（借 Initials 档）。
        let matched = match_name("微信", "wxin").unwrap();
        assert_eq!(matched.kind, PinyinMatchKind::Initials);
        assert_eq!(matched.class, 0);
        assert_eq!(matched.spans, [0, 2]);
        // 中段混用：起点 1 → class 2，spans 只覆盖消费段。
        let matched = match_name("我的网易云音乐", "wangyiyy").unwrap();
        assert_eq!(matched.class, 2);
        assert_eq!(matched.position, 2);
    }

    /// S3：超长查询（> 63 字节）跳过混用、不 panic，退回两条纯路径。
    #[test]
    fn s3_overlong_query_skips_mixed_without_panic() {
        let long = "w".repeat(64);
        let (tokens, has_han) = encode_tokens("微信");
        assert!(has_han);
        assert!(match_tokens_mixed(&tokens, long.as_bytes()).is_none());
        let encoded = encode_compact("微信").unwrap();
        assert!(match_compact_mixed(&encoded, long.as_bytes()).is_none());
        // 63 字节在混用范围内正常工作（不命中也不 panic）。
        assert!(match_compact_mixed(&encoded, "w".repeat(63).as_bytes()).is_none());
    }

    /// S3：sidecar 紧凑编码与即时编码两条路径的混用判定等价
    ///（apps.rs 有同款锚定；这里直接覆盖 compact 侧的混用正例）。
    #[test]
    fn s3_compact_mixed_matches_live_path() {
        for (name, query) in [("微信", "wxin"), ("网易云音乐", "wangyiyy"), ("微信开发", "wxkaifa")] {
            let encoded = encode_compact(name).unwrap();
            let compact = match_compact(&encoded, query).unwrap();
            let live = match_name(name, query).unwrap();
            assert_eq!(compact.kind, live.kind, "{name}/{query}");
            assert_eq!(compact.class, live.class, "{name}/{query}");
            assert_eq!(compact.position, live.position, "{name}/{query}");
            assert_eq!(compact.spans, live.spans, "{name}/{query}");
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

    /// P1-1（搜索报告2，2026-08-21）：词表扩充后新词条可命中、
    /// 老词条与默认读音路径不回归、负例不放宽。
    #[test]
    fn p1_1_expanded_phrases_match_and_defaults_do_not_regress() {
        let positive = [
            // 新增词条（此前走错读音或不命中）
            ("成长", "chengzhang"),
            ("睡觉", "shuijiao"),
            ("主角", "zhujue"),
            ("倔强", "juejiang"),
            ("模样", "muyang"),
            ("模板", "muban"),
            ("出差", "chuchai"),
            ("大夫", "daifu"),
            ("西藏", "xizang"),
            ("血压", "xueya"),
            ("外壳", "waike"),
            ("传记", "zhuanji"),
            ("曾经", "cengjing"),
            ("六安", "luan"),
            ("番禺", "panyu"),
            ("重启", "chongqi"),
            ("弹窗", "tanchuang"),
            ("调整", "tiaozheng"),
            ("调用", "diao yong"),
            ("都市", "dushi"),
            ("都是", "doushi"),
            ("水流传输", "shuiliuchuanshu"),
            ("有限公司董事长", "dongshizhang"),
            // 老词条回归
            ("重庆", "chongqing"),
            ("银行", "yinhang"),
            ("角色", "juese"),
        ];
        for (name, query) in positive {
            assert!(match_name(name, query).is_some(), "{name} / {query}");
        }

        // 负例：词表不得把默认读音改错——「重要」仍是 zhong 不是 chong，
        // 「行为」仍是 xingwei（hang 只在行市/称谓词里），「模型」仍是 mo。
        assert!(match_name("重要", "chongyao").is_none());
        assert!(match_name("行为", "hangwei").is_none());
        assert!(
            match_name("模型", "muxing").is_none(),
            "模型=mo xing，模样/模板=mu"
        );
        assert!(match_name("重要", "zhongyao").is_some());
        assert!(match_name("行为", "xingwei").is_some());
        assert!(match_name("模型", "moxing").is_some());
    }
}
