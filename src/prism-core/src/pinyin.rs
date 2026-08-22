//! Versioned pinyin encoding and matching shared by broker and indexer.

pub const PINYIN_DICTIONARY_VERSION: &str =
    "pinyin-0.10.0/pinyin-data-0.13.0+prism-phrases-v2b+heteronym";

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

/// P1-2（搜索报告2，2026-08-21）：token 携带**全部**读音（多音字经
/// pinyin crate 的 heteronym 表；PHRASES 命中的字仍锁定词表读音——词表
/// 是人工核对过的，比机器多读音更可信）。匹配路径对多读音 token 逐读
/// 音分支尝试。`readings` 去重且上限 4（异体生僻读音过多只会撑爆 DP 分支）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Token {
    readings: Vec<String>,
    utf16_start: u16,
    utf16_len: u16,
}

impl Token {
    fn first_reading(&self) -> &str {
        self.readings
            .first()
            .map(String::as_str)
            .unwrap_or_default()
    }
}

fn tokens_initials(tokens: &[Token], out: &mut Vec<u8>) {
    out.clear();
    for token in tokens {
        let first = token.first_reading();
        if let Some(byte) = first.as_bytes().first() {
            out.push(*byte);
        }
    }
}

/// P1-2：单 token 最多收录的读音数。
const MAX_READINGS_PER_TOKEN: usize = 4;

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
    // P3（搜索报告2，2026-08-21）：2~3 个空白分词各自有效时按 AND 多 term
    // 匹配（每 term 独立过三策略+链，全部命中才算数）；其余情况整串单口径。
    if let Some(terms) = split_query_terms(query) {
        let mut parts = Vec::with_capacity(terms.len());
        for term in &terms {
            parts.push(match_name_single(name, term)?);
        }
        return Some(combine_term_matches(parts));
    }
    match_name_single(name, query)
}

fn match_name_single(name: &str, query: &str) -> Option<PinyinMatch> {
    let query = normalize_query(query)?;
    let (tokens, has_han) = encode_tokens(name);
    if !has_han {
        return None;
    }
    match_tokens(&tokens, query.as_bytes(), PinyinMatchKind::Full)
        .or_else(|| match_tokens(&tokens, query.as_bytes(), PinyinMatchKind::Initials))
        .or_else(|| match_tokens_mixed(&tokens, query.as_bytes()))
}

/// P3：把原始查询按空白切成 2~3 个**全部有效**（各自通过 normalize_query）
/// 的拼音 term；返回 None 表示走整串单口径（单 term / 超限 / 含无效 term）。
pub(crate) fn split_query_terms(query: &str) -> Option<Vec<String>> {
    let terms: Vec<&str> = query.split_whitespace().collect();
    if !(2..=3).contains(&terms.len()) {
        return None;
    }
    let mut normalized = Vec::with_capacity(terms.len());
    for term in terms {
        normalize_query(term)?;
        normalized.push(term.to_owned());
    }
    Some(normalized)
}

/// P3：合并各 term 的命中——kind 取最优（Full>Initials）、class/position 取
/// 最优、score 取最大；spans 拼接后按起点排序、重叠/相邻合并（对齐 S4
/// 字面侧的 span 口径）。
pub(crate) fn combine_term_matches(mut parts: Vec<PinyinMatch>) -> PinyinMatch {
    if parts.len() == 1 {
        return parts.pop().unwrap_or_else(|| PinyinMatch {
            kind: PinyinMatchKind::Initials,
            class: 2,
            position: 0,
            score: 0,
            spans: Vec::new(),
        });
    }
    let kind = parts
        .iter()
        .find(|part| part.kind == PinyinMatchKind::Full)
        .map_or(PinyinMatchKind::Initials, |part| part.kind);
    let class = parts.iter().map(|part| part.class).min().unwrap_or(2);
    let position = parts.iter().map(|part| part.position).min().unwrap_or(0);
    let score = parts.iter().map(|part| part.score).max().unwrap_or(0);
    let mut ranges: Vec<(i32, i32)> = Vec::new();
    for part in &parts {
        let mut index = 0;
        while index + 1 < part.spans.len() {
            ranges.push((part.spans[index], part.spans[index + 1]));
            index += 2;
        }
    }
    ranges.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut spans: Vec<i32> = Vec::with_capacity(ranges.len() * 2);
    for (start, len) in ranges {
        if len <= 0 {
            continue;
        }
        if spans.len() >= 2 && spans[spans.len() - 2] + spans[spans.len() - 1] >= start {
            let last = spans.len() - 1;
            let last_start = spans[last - 1];
            let last_end = last_start + spans[last];
            let end = (start + len).max(last_end);
            spans[last] = end - last_start;
        } else {
            spans.push(start);
            spans.push(len);
        }
    }
    PinyinMatch {
        kind,
        class,
        position,
        score,
        spans,
    }
}

/// P1-2+P2（搜索报告2，2026-08-21）：紧凑编码 v2。
/// 记录布局：`chain_len u16le | chain 首字母字节 | kind u8 | body`。
/// kind=0（汉字名）：token 流，token = `total_len u8 | utf16_start u16le |
/// utf16_len u16le | rcount u8 | (r_len u8, r)*`（P1-2 多读音）。
/// kind=1 保留值（曾用于「纯 ASCII 名 + 链」记录体——真机实测 2.4M 记录
/// 把 sidecar 撑到 98MB、索引进程击穿 100MB 内存门，已停用；解析端保留
/// 分支以稳妥读取历史产物）。纯 ASCII 名回到字面搜索通道。
/// chain = 祖先目录名的首字母串（root→parent，仅目录）。
/// sidecar 的 SCHEMA_VERSION 与文件名随本格式一并 bump（v1 文件按 Missing 重建）。
pub(crate) fn encode_compact(name: &str) -> Option<Vec<u8>> {
    encode_compact_with_chain(name, &[])
}

pub(crate) fn encode_compact_with_chain(name: &str, chain: &[u8]) -> Option<Vec<u8>> {
    let (tokens, has_han) = encode_tokens(name);
    if !has_han {
        // 纯 ASCII 名：字面搜索已覆盖（见上方 kind=1 注释——曾试过带链收录，
        // 内存代价比场景价值高一个量级）。
        return None;
    }
    let mut bytes = Vec::new();
    let chain_len = u16::try_from(chain.len()).ok()?;
    bytes.extend_from_slice(&chain_len.to_le_bytes());
    bytes.extend_from_slice(chain);
    bytes.push(0);
    for token in tokens {
        // total_len = utf16 字段(4) + rcount(1) + Σ(1+r_len)
        let total = 4
            + 1
            + token
                .readings
                .iter()
                .map(|reading| 1 + reading.len())
                .sum::<usize>();
        let total = u8::try_from(total).ok()?;
        bytes.push(total);
        bytes.extend_from_slice(&token.utf16_start.to_le_bytes());
        bytes.extend_from_slice(&token.utf16_len.to_le_bytes());
        let count = u8::try_from(token.readings.len()).ok()?;
        bytes.push(count);
        for reading in &token.readings {
            let len = u8::try_from(reading.len()).ok()?;
            bytes.push(len);
            bytes.extend_from_slice(reading.as_bytes());
        }
    }
    Some(bytes)
}

/// P2：记录体（chain 之后的部分）。
enum CompactBody<'a> {
    /// 汉字名：token 流。
    Tokens(&'a [u8]),
    /// 纯 ASCII 名：名字 ASCII 字节（链匹配专用，名字策略跳过）。
    AsciiName(&'a [u8]),
}

impl CompactBody<'_> {
    fn tokens(&self) -> Option<&[u8]> {
        match self {
            CompactBody::Tokens(bytes) => Some(bytes),
            CompactBody::AsciiName(_) => None,
        }
    }
}

/// P2：v2 记录头拆分（链 + 记录体）。
fn compact_split_record(bytes: &[u8]) -> Option<(&[u8], CompactBody<'_>)> {
    let chain_len = usize::from(u16::from_le_bytes([*bytes.first()?, *bytes.get(1)?]));
    let chain = bytes.get(2..2 + chain_len)?;
    let body = bytes.get(2 + chain_len..)?;
    match body.first() {
        Some(0) => Some((chain, CompactBody::Tokens(body.get(1..)?))),
        Some(1) => Some((chain, CompactBody::AsciiName(body.get(1..)?))),
        _ => None,
    }
}

/// P2：名字首字母串（汉字名取每 token 第一读音首字节；ASCII 记录体即名字字节）。
/// pub(crate) 供 sidecar 构建目录链时逐目录名复用。
pub(crate) fn name_initials(name: &str, out: &mut Vec<u8>) {
    let (tokens, _) = encode_tokens(name);
    tokens_initials(&tokens, out);
}

/// P1-2：名字是否含汉字（delta 阈值计数用——纯 ASCII 名 + 中文链不计数，
/// 与 M3「英文风暴不触发重建」的语义一致）。
pub(crate) fn name_has_han(name: &str) -> bool {
    name.chars().any(|ch| {
        ('\u{3400}'..='\u{9FFF}').contains(&ch) || ('\u{F900}'..='\u{FAFF}').contains(&ch)
    })
}

fn token_initials(body: &CompactBody<'_>, scratch: &mut Vec<u8>) -> Option<()> {
    scratch.clear();
    match body {
        CompactBody::AsciiName(name) => {
            scratch.extend_from_slice(name);
        }
        CompactBody::Tokens(tokens_bytes) => {
            let mut cursor = 0usize;
            while cursor < tokens_bytes.len() {
                let token = compact_token(tokens_bytes, cursor)?;
                scratch.push(*compact_readings(&token).next()?.first()?);
                cursor = token.next;
            }
        }
    }
    Some(())
}

#[cfg(test)]
pub(crate) fn match_compact(bytes: &[u8], query: &str) -> Option<PinyinMatch> {
    let query = normalize_query(query)?;
    let mut scratch = Vec::new();
    match_compact_normalized(bytes, query.as_bytes(), &mut scratch)
}

/// P3：多 term 记录匹配（compact）——每 term 独立过全部策略，全部命中才
/// 返回合并结果。terms 由 `split_query_terms` 预先校验（各 term 必有效）。
pub(crate) fn match_compact_terms(
    bytes: &[u8],
    terms: &[String],
    scratch: &mut Vec<u8>,
) -> Option<PinyinMatch> {
    let mut parts = Vec::with_capacity(terms.len());
    for term in terms {
        let normalized = normalize_query(term)?;
        parts.push(match_compact_normalized(
            bytes,
            normalized.as_bytes(),
            scratch,
        )?);
    }
    Some(combine_term_matches(parts))
}

/// `scratch` 是名字首字母 scratch（链匹配用），调用方在扫描循环外持有复用。
pub(crate) fn match_compact_normalized(
    bytes: &[u8],
    query: &[u8],
    scratch: &mut Vec<u8>,
) -> Option<PinyinMatch> {
    match_compact_kind(bytes, query, PinyinMatchKind::Full)
        .or_else(|| match_compact_kind(bytes, query, PinyinMatchKind::Initials))
        // S3（PRISM-IMPL-PLAN-4-2026-08-20）：前两条纯策略都失败才试混用。
        .or_else(|| match_compact_mixed(bytes, query))
        // P2（搜索报告2，2026-08-21）：名字三策略未中再试目录链首字母
        //（纯首字母连续段，起点在链内，可止于链内或延伸进名字前缀）。
        .or_else(|| match_compact_chain(bytes, query, scratch))
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
    // P2：ASCII 名记录体（kind=1）只服务链匹配，名字策略跳过。
    let (_, body) = compact_split_record(bytes)?;
    let tokens_bytes = body.tokens()?;
    let done_bit = 1u64 << query.len();
    let mut start_cursor = 0usize;
    let mut start_index = 0u32;
    while start_cursor < tokens_bytes.len() {
        let mut mask = 1u64;
        let mut cursor = start_cursor;
        while cursor < tokens_bytes.len() && mask != 0 {
            let token = compact_token(tokens_bytes, cursor)?;
            let mut next = 0u64;
            let mut bits = mask;
            while bits != 0 {
                let p = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                // P1-2：多读音 token 对每个读音各试一次转移。
                for reading in compact_readings(&token) {
                    // 终态：reading 以剩余查询为前缀（尾部部分匹配，允许名字尾部未覆盖）。
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
            }
            if next & done_bit != 0 {
                let end_cursor = token.next;
                // class 0 只在「起点为 0 + 覆盖到名字末尾 + 最后消费是全拼」时给，
                // 与两条纯路径的 at_end 语义对齐（前一条状态含对应全拼位）。
                let full_at_end = compact_readings(&token).any(|reading| {
                    end_cursor == tokens_bytes.len()
                        && query.len() >= reading.len()
                        && mask & (1u64 << (query.len() - reading.len())) != 0
                });
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
                    score: compact_token_count(tokens_bytes)?,
                    spans: compact_spans(tokens_bytes, start_cursor, end_cursor)?,
                });
            }
            mask = next;
            cursor = token.next;
        }
        let token = compact_token(tokens_bytes, start_cursor)?;
        start_cursor = token.next;
        start_index = start_index.saturating_add(1);
    }
    None
}

/// P2（搜索报告2，2026-08-21）：目录链首字母匹配——纯首字母串
/// `chain + 名字首字母` 中，起点落在链内的连续段命中（可止于链内
///——「按目录名搜到该目录下文件」，或延伸覆盖名字前缀）。
/// class 恒 2（链辅助命中低于名字本身命中），kind 借 Initials 档。
/// 命中时 spans 覆盖名字侧被消费的前缀 token（全在链内则为空 spans）。
/// `name_initials` 是调用方复用的 scratch（热路径零分配）。
fn match_compact_chain(
    bytes: &[u8],
    query: &[u8],
    name_initials_scratch: &mut Vec<u8>,
) -> Option<PinyinMatch> {
    let (chain, body) = compact_split_record(bytes)?;
    if chain.is_empty() {
        return None;
    }
    token_initials(&body, name_initials_scratch)?;
    let tokens_bytes = body.tokens();
    let name_initials = name_initials_scratch.as_slice();
    let full_len = chain.len() + name_initials.len();
    if query.len() > full_len {
        return None;
    }
    for start in 0..chain.len() {
        // 先在链内连续消费。
        let mut qi = 0usize;
        while start + qi < chain.len() && qi < query.len() && chain[start + qi] == query[qi] {
            qi += 1;
        }
        if qi == query.len() {
            return Some(PinyinMatch {
                kind: PinyinMatchKind::Initials,
                class: 2,
                position: 0,
                score: tokens_bytes
                    .and_then(compact_token_count)
                    .unwrap_or(name_initials.len() as u32),
                spans: Vec::new(),
            });
        }
        // 只允许消费到链尾后**连续**延伸进名字首字母前缀。
        if start + qi != chain.len() {
            continue;
        }
        match &body {
            CompactBody::AsciiName(ascii_name) => {
                // 纯 ASCII 名：延伸段就是名字字节前缀（spans 从简为空）。
                if ascii_name.starts_with(&query[qi..]) {
                    return Some(PinyinMatch {
                        kind: PinyinMatchKind::Initials,
                        class: 2,
                        position: 0,
                        score: ascii_name.len() as u32,
                        spans: Vec::new(),
                    });
                }
            }
            CompactBody::Tokens(tokens_bytes) => {
                let mut qi = qi;
                let mut cursor = 0usize;
                while cursor < tokens_bytes.len() && qi < query.len() {
                    let token = compact_token(tokens_bytes, cursor)?;
                    let initial = *compact_readings(&token).next()?.first()?;
                    if initial != query[qi] {
                        break;
                    }
                    qi += 1;
                    cursor = token.next;
                }
                if qi == query.len() {
                    return Some(PinyinMatch {
                        kind: PinyinMatchKind::Initials,
                        class: 2,
                        position: 0,
                        score: compact_token_count(tokens_bytes)?,
                        spans: compact_spans(tokens_bytes, 0, cursor)?,
                    });
                }
            }
        }
    }
    None
}

fn encode_tokens(name: &str) -> (Vec<Token>, bool) {
    use ::pinyin::ToPinyinMulti;
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
                // 词表读音是人工核对的单一读音（比机器多音表更可信）。
                tokens.push(Token {
                    readings: vec![(*reading).to_owned()],
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
        if let Some(multi) = ch.to_pinyin_multi() {
            has_han = true;
            // P1-2：收录全部读音（去重、去空、封顶）。first 与 to_pinyin() 同源，
            // 单读音行为完全不变；多音字新增分支只在单读不命中时兜底。
            let mut readings: Vec<String> = Vec::with_capacity(1);
            for pinyin in multi.into_iter() {
                let reading = normalize_reading(pinyin.plain());
                if reading.is_empty()
                    || readings.len() >= MAX_READINGS_PER_TOKEN
                    || readings.contains(&reading)
                {
                    continue;
                }
                readings.push(reading);
            }
            if !readings.is_empty() {
                tokens.push(Token {
                    readings,
                    utf16_start,
                    utf16_len: len,
                });
            }
        } else if ch.is_ascii_alphanumeric() {
            tokens.push(Token {
                readings: vec![ch.to_ascii_lowercase().to_string()],
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
    bytes: &'a [u8],
    readings_cursor: usize,
    reading_count: usize,
    utf16_start: u16,
    utf16_len: u16,
    next: usize,
}

/// P1-2：本 token 的全部读音切片（无分配——按 r_len 逐个切）。
fn compact_readings<'a>(token: &CompactToken<'a>) -> CompactReadings<'a> {
    CompactReadings {
        bytes: token.bytes,
        cursor: token.readings_cursor,
        remaining: token.reading_count,
    }
}

struct CompactReadings<'a> {
    bytes: &'a [u8],
    cursor: usize,
    remaining: usize,
}

impl<'a> Iterator for CompactReadings<'a> {
    type Item = &'a [u8];
    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let len = *self.bytes.get(self.cursor)? as usize;
        let start = self.cursor + 1;
        let end = start.checked_add(len)?;
        let reading = self.bytes.get(start..end)?;
        self.cursor = end;
        self.remaining -= 1;
        Some(reading)
    }
}

fn compact_token(bytes: &[u8], cursor: usize) -> Option<CompactToken<'_>> {
    // P1-2 v2 布局：total_len u8 | utf16_start u16 | utf16_len u16 | rcount u8 | (r_len u8, r)*
    let total = *bytes.get(cursor)? as usize;
    if total < 5 {
        return None;
    }
    let utf16_start = u16::from_le_bytes([*bytes.get(cursor + 1)?, *bytes.get(cursor + 2)?]);
    let utf16_len = u16::from_le_bytes([*bytes.get(cursor + 3)?, *bytes.get(cursor + 4)?]);
    let rcount = *bytes.get(cursor + 5)? as usize;
    let readings_cursor = cursor + 6;
    let next = readings_cursor.checked_add(total - 5)?;
    let readings_region = bytes.get(readings_cursor..next)?;
    if rcount == 0 || readings_region.is_empty() {
        return None;
    }
    // 逐读音校验长度链一致 + 非空（坏数据宁可不命中，绝不 panic——release
    // 是 abort）。M4（复审 2026-08-21）：此前只校验长度链总和，r_len==0 的
    // 空读音能通过——Initials 分支的 `&reading[..1]` 与混用 DP 的
    // `reading[0]` 都是越界 panic。
    let mut walk = 0usize;
    for _ in 0..rcount {
        let len = *readings_region.get(walk)? as usize;
        if len == 0 {
            return None;
        }
        walk = walk.checked_add(1 + len)?;
    }
    if walk != readings_region.len() {
        return None;
    }
    (utf16_len > 0).then_some(CompactToken {
        bytes,
        readings_cursor,
        reading_count: rcount,
        utf16_start,
        utf16_len,
        next,
    })
}

/// P1-2（搜索报告2，2026-08-21）：纯策略（全拼/首字母）多读音版。
/// 每 token 对每个读陚分支推进——可达状态集合 = 已消费 query 字节数
///（≤ query.len+1 个，位置集很小，`Vec<usize>` 即可）。
/// 语义对齐单读音版：按起点升序，命中发生在最早的 token，返回时优先
/// at_end=true 的命中（class 更优）。
fn match_compact_kind(bytes: &[u8], query: &[u8], kind: PinyinMatchKind) -> Option<PinyinMatch> {
    // P2：ASCII 名记录体（kind=1）只服务链匹配，名字策略跳过。
    let (_, body) = compact_split_record(bytes)?;
    let tokens_bytes = body.tokens()?;
    let mut start_cursor = 0usize;
    let mut start_index = 0u32;
    let mut positions: Vec<usize> = Vec::with_capacity(8);
    let mut next_positions: Vec<usize> = Vec::with_capacity(8);
    while start_cursor < tokens_bytes.len() {
        positions.clear();
        positions.push(0usize);
        let mut cursor = start_cursor;
        let mut hit: Option<(usize, bool)> = None; // (end_cursor, at_end)
        while cursor < tokens_bytes.len() {
            let token = compact_token(tokens_bytes, cursor)?;
            next_positions.clear();
            let mut at_end_any = false;
            for &query_at in &positions {
                for reading in compact_readings(&token) {
                    let reading = match kind {
                        PinyinMatchKind::Full => reading,
                        PinyinMatchKind::Initials => &reading[..1],
                    };
                    let remaining = &query[query_at..];
                    let compared = remaining.len().min(reading.len());
                    if remaining[..compared] != reading[..compared] {
                        continue;
                    }
                    let advanced = query_at + compared;
                    if advanced == query.len() {
                        let at_end = token.next == tokens_bytes.len() && compared == reading.len();
                        if at_end {
                            at_end_any = true;
                        }
                        if hit.is_none() || (at_end && !hit.is_some_and(|(_, end)| end)) {
                            hit = Some((token.next, at_end));
                        }
                        continue;
                    }
                    if compared != reading.len() {
                        continue;
                    }
                    if !next_positions.contains(&advanced) {
                        next_positions.push(advanced);
                    }
                }
            }
            if hit.is_some() {
                break;
            }
            if at_end_any || next_positions.is_empty() {
                break;
            }
            std::mem::swap(&mut positions, &mut next_positions);
            cursor = token.next;
        }
        if let Some((end, at_end)) = hit {
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
                score: compact_token_count(tokens_bytes)?,
                spans: compact_spans(tokens_bytes, start_cursor, end)?,
            });
        }
        let token = compact_token(tokens_bytes, start_cursor)?;
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

/// P1-2：即时路径的纯策略（全拼/首字母）多读音版——位置集分支，
/// 语义与 compact 侧逐字节对齐（apps.rs 等价锚保两条路径判定一致）。
fn match_tokens(tokens: &[Token], query: &[u8], kind: PinyinMatchKind) -> Option<PinyinMatch> {
    for start in 0..tokens.len() {
        let mut positions: Vec<usize> = vec![0];
        let mut hit: Option<(usize, bool)> = None; // (end exclusive, at_end)
        for (offset, token) in tokens[start..].iter().enumerate() {
            let mut next_positions: Vec<usize> = Vec::with_capacity(positions.len());
            for &query_at in &positions {
                for reading in &token.readings {
                    let reading: &[u8] = match kind {
                        PinyinMatchKind::Full => reading.as_bytes(),
                        PinyinMatchKind::Initials => &reading.as_bytes()[..1],
                    };
                    let remaining = &query[query_at..];
                    let compared = remaining.len().min(reading.len());
                    if remaining[..compared] != reading[..compared] {
                        continue;
                    }
                    let advanced = query_at + compared;
                    if advanced == query.len() {
                        let end = start + offset + 1;
                        let at_end = end == tokens.len() && compared == reading.len();
                        if hit.is_none() || (at_end && !hit.is_some_and(|(_, end)| end)) {
                            hit = Some((end, at_end));
                        }
                        continue;
                    }
                    if compared != reading.len() {
                        continue;
                    }
                    if !next_positions.contains(&advanced) {
                        next_positions.push(advanced);
                    }
                }
            }
            if hit.is_some() || next_positions.is_empty() {
                break;
            }
            positions = next_positions;
        }
        if let Some((end, at_end)) = hit {
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
            let mut next = 0u64;
            let mut bits = mask;
            while bits != 0 {
                let p = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                // P1-2：多读音 token 对每个读音各试一次转移。
                for reading_bytes in token.readings.iter().map(String::as_bytes) {
                    if reading_bytes.starts_with(&query[p..]) {
                        next |= done_bit;
                    }
                    if reading_bytes[0] == query[p] {
                        next |= 1u64 << (p + 1);
                    }
                    if query[p..].starts_with(reading_bytes) {
                        next |= 1u64 << (p + reading_bytes.len());
                    }
                }
            }
            if next & done_bit != 0 {
                let end = start + offset + 1;
                let full_at_end = token.readings.iter().any(|reading| {
                    let reading = reading.as_bytes();
                    end == tokens.len()
                        && query.len() >= reading.len()
                        && mask & (1u64 << (query.len() - reading.len())) != 0
                });
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

    /// 复审 L3（2026-08-21）：encode_tokens 的词表分支按 readings.len() 索引
    /// `chars[index + offset]`——词表条目一旦出现「字数 ≠ 读音数」（未来的
    /// 手工编辑），首个含该词的文件名即越界 panic（release 下 abort 整个
    /// 索引服务）。锚死不变量：改词表必过此测试（词表变更本就要求
    /// PINYIN_DICTIONARY_VERSION bump）。
    #[test]
    fn phrases_table_char_count_matches_readings_count() {
        for (phrase, readings) in PHRASES {
            assert_eq!(
                phrase.chars().count(),
                readings.len(),
                "PHRASES 条目「{phrase}」字数({})必须等于读音数({})，否则 encode_tokens 越界",
                phrase.chars().count(),
                readings.len(),
            );
            for reading in *readings {
                assert!(!reading.is_empty(), "PHRASES 条目「{phrase}」不得有空读音");
            }
        }
    }

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
        for (name, query) in [
            ("微信", "wxin"),
            ("网易云音乐", "wangyiyy"),
            ("微信开发", "wxkaifa"),
        ] {
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

        // P1-2（搜索报告2）：多读音语义——词表外的多音字按**任一有效读音**
        // 组合均可命中（对齐 pinyin-match/IbEverythingExt 的 heteronym 语义）。
        // 重要=zhongyao，但 重 另有 chong 读音；行为 xingwei/hangwei 同理。
        // 「按目录名搜文件」的精确语义项交给 PHRASES 词表（银行=hang 等）。
        assert!(match_name("重要", "zhongyao").is_some());
        assert!(
            match_name("重要", "chongyao").is_some(),
            "多读音分支：重 chong 有效"
        );
        assert!(match_name("行为", "xingwei").is_some());
        assert!(
            match_name("行为", "hangwei").is_some(),
            "多读音分支：行 hang 有效"
        );
        assert!(match_name("模型", "moxing").is_some());
        assert!(
            match_name("模型", "muxing").is_some(),
            "多读音分支：模 mu 有效（模样/模板）"
        );
        // 负例：不是该字任何读音的音节绝不命中（防召回无界放宽）。
        assert!(match_name("重庆", "hongqing").is_none());
        assert!(match_name("银行", "yinkuan").is_none());
    }

    /// 复审 M4（2026-08-21）：r_len==0 的空读音记录必须被 compact_token 拒绝
    ///——Initials 分支的 `&reading[..1]` 与混用 DP 的 `reading[0]` 都是越界
    /// panic（release 是 abort，一条坏记录杀整个索引服务）。
    #[test]
    fn m4_zero_length_reading_is_rejected_without_panic() {
        // 手工构造：chain_len=0 | kind=0 | token(total=5+2, utf16, rcount=1, r_len=0)
        let mut bytes = vec![0, 0, 0];
        bytes.push(5 + 2); // total
        bytes.extend_from_slice(&0u16.to_le_bytes()); // utf16_start
        bytes.extend_from_slice(&1u16.to_le_bytes()); // utf16_len
        bytes.push(1); // rcount
        bytes.push(0); // r_len = 0 —— 毒丸
        let mut scratch = Vec::new();
        assert!(match_compact_normalized(&bytes, b"wx", &mut scratch).is_none());
    }

    /// P1-2（搜索报告2，2026-08-21）：heteronym 多读音端到端——
    /// 露 在词表外，第一读 lù(lu) 之外另有 lòu(lou)；「露脸」的 loulian
    /// 在单读音时代不命中，现在必须命中，且默认读 lulian 不回归。
    #[test]
    fn p1_2_heteronym_readings_match() {
        let lou = match_name("露脸", "loulian");
        assert!(lou.is_some(), "露=lòu 读法必须经多读音分支命中");
        let lu = match_name("露脸", "lulian");
        assert!(lu.is_some(), "第一读 lù 不回归");
    }

    /// P3（搜索报告2，2026-08-21）：拼音多 term（2~3 个空白分词 AND）。
    /// 每 term 独立过策略，全部命中才算数；任一 term 落空整条不中。
    #[test]
    fn p3_multi_term_pinyin_and_semantics() {
        // wx + bg：微信报告（w x + bao gao）两段都命中。
        let hit = match_name("微信报告", "wx bg").unwrap();
        assert_eq!(hit.spans, [0, 4], "两个 term 的 spans 合并覆盖全部四字");
        // 任一 term 落空：微信报告没有 zz 段。
        assert!(match_name("微信报告", "wx zz").is_none());
        // term 顺序无关（AND 语义）：bg wx 同样命中。
        assert!(match_name("微信报告", "bg wx").is_some());
        // 单 term / 超限（4 term）回退整串单口径：4 term 归一后按整串匹配
        //（"wx bg zz qq" 整串不可能命中微信报告）。
        assert!(match_name("微信报告", "wx bg zz qq").is_none());
        // 无效 term（单字母 w）不启用多 term：整串口径下 "w bg" 归一为
        // "wbg" 不命中。
        assert!(match_name("微信报告", "w bg").is_none());
        // 混用策略在 term 内照常工作：xin（全拼）+ bg（首字母）。
        assert!(match_name("微信报告", "xin bg").is_some());
        // 三 term AND。
        assert!(match_name("微信支付宝报告", "wx zfb bg").is_some());
        assert!(match_name("微信支付宝报告", "wx zfb zz").is_none());
    }

    /// P2（搜索报告2，2026-08-21）：目录链首字母匹配。
    /// `下载\资料` 链 = "xz"+"zl"；查询可止于链内（按目录名搜到文件），
    /// 也可连续延伸覆盖名字首字母前缀。class 恒 2、kind 借 Initials 档。
    #[test]
    fn p2_directory_chain_initials_match() {
        // 下载(xz) → 资料(zl) → 资料.pdf（名字首字母 zl + pdf）
        let encoded = encode_compact_with_chain("资料.pdf", b"xzzl").unwrap();
        let mut scratch = Vec::new();
        // 止于链内：xz 命中该文件（空 spans——名字侧无消费）。
        let chain_only = match_compact_normalized(&encoded, b"xz", &mut scratch).unwrap();
        assert_eq!(chain_only.class, 2);
        assert!(chain_only.spans.is_empty());
        // 链尾延伸进名字前缀：xzzlz（链 xzzl + 名字首字「资」z）覆盖名字 [0,1)。
        let spill = match_compact_normalized(&encoded, b"xzzlz", &mut scratch).unwrap();
        assert_eq!(spill.class, 2);
        assert_eq!(spill.spans, [0, 1], "资 是 BMP 字符，UTF-16 [0,1)");
        // 链不连续/不匹配：qq 与链与名字首字母都对不上。
        assert!(match_compact_normalized(&encoded, b"qq", &mut scratch).is_none());
        // 纯 ASCII 名不再入拼音通道（真机实测带链收录会把 sidecar 撑爆内存门，
        // 见 encode_compact_with_chain 注释）——字面搜索覆盖。
        assert!(encode_compact_with_chain("report.pdf", b"xz").is_none());
    }
}
