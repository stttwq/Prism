//! 网页快捷搜索：解析 `g 天气` 类输入，匹配引擎关键词并生成 URL。
//!
//! 预设与前端 `Settings.DefaultEngines` 一致：bi=Bing（优先）、b=百度、g=Google。
//! 自定义引擎从共享 `settings.json` 的 `WebEngines` 字段加载（见 `config`）。
//! 匹配时按关键词长度降序，保证 `bi` 优先于 `b`。

use serde::{Deserialize, Serialize};

use crate::ipc::{SearchResult, SearchResultKind};

/// 一个网页搜索引擎（与前端 `WebEngine` record 字段对齐）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebEngine {
    /// 触发关键词（如 "g"），匹配时不区分大小写。
    #[serde(alias = "Keyword")]
    pub keyword: String,
    /// 显示名（如 "Google" / "百度"）。
    #[serde(alias = "Name")]
    pub name: String,
    /// URL 模板，用 `{q}` 占位查询词。
    /// 兼容 snake_case / camelCase / PascalCase（前端 SettingsStore 默认 PascalCase）。
    #[serde(alias = "UrlTemplate", alias = "urlTemplate")]
    pub url_template: String,
}

impl WebEngine {
    /// 与 C# `Settings.DefaultEngines()` 相同的三个预设。
    /// 顺序：必应优先（国内可测），其次百度、Google。
    pub fn defaults() -> Vec<Self> {
        vec![
            Self {
                keyword: "bi".into(),
                name: "Bing".into(),
                url_template: "https://www.bing.com/search?q={q}".into(),
            },
            Self {
                keyword: "b".into(),
                name: "百度".into(),
                url_template: "https://www.baidu.com/s?wd={q}".into(),
            },
            Self {
                keyword: "g".into(),
                name: "Google".into(),
                url_template: "https://www.google.com/search?q={q}".into(),
            },
        ]
    }
}

/// 匹配成功后的网页结果（尚未序列化为 IPC `SearchResult`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebHit {
    pub engine_name: String,
    pub query_terms: String,
    pub url: String,
}

impl WebHit {
    /// 转为前端可渲染的搜索结果：kind=web，execute_id=最终 URL。
    pub fn into_search_result(self) -> SearchResult {
        let title = if self.query_terms.is_empty() {
            format!("在 {} 中搜索", self.engine_name)
        } else {
            format!("在 {} 中搜索：{}", self.engine_name, self.query_terms)
        };
        // 副标题展示最终 URL，便于用户确认将打开的地址。
        let subtitle = self.url.clone();
        let target =
            crate::shell::ActionTarget::new(crate::shell::TargetKind::Web, self.url.clone());
        let match_spans = match_spans_in_title(&title, &self.query_terms);
        SearchResult {
            kind: SearchResultKind::Web,
            title,
            subtitle,
            execute_id: self.url,
            target,
            match_spans,
            match_metadata: None,
        }
    }
}

/// 尝试把用户输入解析为网页搜索。
///
/// 规则：
/// - 取首个空白分隔 token 作为关键词，其余（trim 后）为查询词；
/// - 关键词与引擎列表逐一比对（忽略 ASCII 大小写）；
/// - 引擎按关键词长度降序匹配，避免 `bi foo` 误命中 `b`。
///
/// 无匹配 / 空查询 / 仅空白 → `None`（走普通文件/程序搜索）。
pub fn try_match(query: &str, engines: &[WebEngine]) -> Option<WebHit> {
    if query.trim().is_empty() || engines.is_empty() {
        return None;
    }

    // Pass the untrimmed query: split_first_token checks for whitespace after the
    // keyword, so "g " enters web mode but "g" does not.
    let (keyword, rest) = split_first_token(query)?;
    if keyword.is_empty() {
        return None;
    }

    // 长关键词优先：bi 在 b 之前。
    let mut order: Vec<usize> = (0..engines.len()).collect();
    order.sort_by(|&i, &j| {
        engines[j]
            .keyword
            .len()
            .cmp(&engines[i].keyword.len())
            .then(i.cmp(&j))
    });

    for idx in order {
        let eng = &engines[idx];
        if eng.keyword.is_empty() {
            continue;
        }
        if eng.keyword.eq_ignore_ascii_case(keyword) {
            let terms = rest.trim();
            let encoded = url_encode(terms);
            let url = eng.url_template.replace("{q}", &encoded);
            return Some(WebHit {
                engine_name: eng.name.clone(),
                query_terms: terms.to_string(),
                url,
            });
        }
    }
    None
}

/// 拆出首 token 与剩余部分。**必须有关键词 + 空白分隔符**才返回 Some——
/// 仅输入 `g` 或 `bi` 不带空格时返回 None，让本地文件搜索正常工作。
/// 例：`g hello` → Some(("g", " hello"))；`g ` → Some(("g", ""))；`g` → None。
fn split_first_token(query: &str) -> Option<(&str, &str)> {
    // Find first non-whitespace (skip leading spaces).
    let start = query.find(|c: char| !c.is_whitespace())?;
    let after_start = &query[start..];
    // There must be whitespace after the keyword.
    let ws = after_start.find(char::is_whitespace)?;
    let keyword = &after_start[..ws];
    let rest = &after_start[ws..];
    if keyword.is_empty() {
        return None;
    }
    Some((keyword, rest))
}

/// application/x-www-form-urlencoded 风格的百分号编码（UTF-8 字节）。
/// 空格编为 `%20`（非 `+`），与常见搜索引擎 URL 兼容。
pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0xf) as usize] as char);
            }
        }
    }
    out
}

/// 判断 execute_id 是否为可交给 ShellExecute 的 http(s) URL。
pub fn is_http_url(id: &str) -> bool {
    let s = id.trim();
    let lower = s.to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://")
}

/// 标题中查询词的 UTF-16 匹配区间（与 ipc::match_spans 约定一致）。
fn match_spans_in_title(title: &str, terms: &str) -> Vec<i32> {
    if terms.is_empty() {
        return Vec::new();
    }
    let title_lower = title.to_lowercase();
    let terms_lower = terms.to_lowercase();
    let Some(byte_pos) = title_lower.find(&terms_lower) else {
        return Vec::new();
    };
    let start_u16 = title_lower[..byte_pos].encode_utf16().count();
    let len_u16 = terms_lower.encode_utf16().count();
    vec![start_u16 as i32, len_u16 as i32]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> Vec<WebEngine> {
        WebEngine::defaults()
    }

    #[test]
    fn g_chinese_builds_google_url() {
        let hit = try_match("g 天气", &defaults()).expect("应命中 Google");
        assert_eq!(hit.engine_name, "Google");
        assert_eq!(hit.query_terms, "天气");
        // 天=E5 A4 A9  气=E6 B0 94
        assert_eq!(
            hit.url,
            "https://www.google.com/search?q=%E5%A4%A9%E6%B0%94"
        );
        let r = hit.into_search_result();
        assert_eq!(r.kind, SearchResultKind::Web);
        assert!(r.execute_id.starts_with("https://www.google.com/"));
        assert!(r.title.contains("Google"));
        assert!(r.title.contains("天气"));
    }

    #[test]
    fn bi_prefers_bing_over_baidu() {
        let hit = try_match("bi foo", &defaults()).expect("应命中 Bing");
        assert_eq!(hit.engine_name, "Bing");
        assert_eq!(hit.url, "https://www.bing.com/search?q=foo");
    }

    #[test]
    fn b_matches_baidu() {
        let hit = try_match("b bar", &defaults()).expect("应命中百度");
        assert_eq!(hit.engine_name, "百度");
        assert_eq!(hit.url, "https://www.baidu.com/s?wd=bar");
    }

    #[test]
    fn keyword_case_insensitive() {
        let hit = try_match("G Hello", &defaults()).expect("大写 G 应命中");
        assert_eq!(hit.engine_name, "Google");
        assert_eq!(hit.query_terms, "Hello");
    }

    #[test]
    fn multiple_spaces_and_chinese() {
        let hit = try_match("  g   今天 天气  ", &defaults()).expect("多空格应可解析");
        assert_eq!(hit.query_terms, "今天 天气");
        assert!(hit.url.contains(&url_encode("今天 天气")));
    }

    #[test]
    fn unknown_keyword_returns_none() {
        assert!(try_match("weather today", &defaults()).is_none());
        assert!(try_match("google 天气", &defaults()).is_none());
    }

    #[test]
    fn empty_or_whitespace_returns_none() {
        assert!(try_match("", &defaults()).is_none());
        assert!(try_match("   ", &defaults()).is_none());
    }

    #[test]
    fn keyword_only_without_space_returns_none() {
        // Keywords without a space must NOT trigger web mode — the user should be
        // able to search local files whose names start with "g", "b", "bi", etc.
        assert!(try_match("g", &defaults()).is_none());
        assert!(try_match("bi", &defaults()).is_none());
        assert!(try_match("b", &defaults()).is_none());
        assert!(try_match("G", &defaults()).is_none());
    }

    #[test]
    fn keyword_with_trailing_space_enters_web_mode() {
        // "g " (trailing space, no terms) → web mode with empty query.
        let hit = try_match("g ", &defaults()).expect("trailing space should enter web mode");
        assert_eq!(hit.engine_name, "Google");
        assert_eq!(hit.query_terms, "");
    }

    #[test]
    fn custom_engine_from_config() {
        let engines = vec![WebEngine {
            keyword: "gh".into(),
            name: "GitHub".into(),
            url_template: "https://github.com/search?q={q}".into(),
        }];
        let hit = try_match("gh rust async", &engines).expect("自定义引擎应命中");
        assert_eq!(hit.engine_name, "GitHub");
        assert_eq!(hit.url, "https://github.com/search?q=rust%20async");
        // 预设关键词在仅有自定义列表时不应命中。
        assert!(try_match("g 天气", &engines).is_none());
    }

    #[test]
    fn is_http_url_detects_schemes() {
        assert!(is_http_url("https://www.google.com/search?q=a"));
        assert!(is_http_url("http://example.com/"));
        assert!(is_http_url("  HTTPS://Example.COM  "));
        assert!(!is_http_url(r"C:\Windows\notepad.exe"));
        assert!(!is_http_url("not\\absolute.txt"));
        assert!(!is_http_url("ftp://files.example/x"));
    }

    #[test]
    fn url_encode_ascii_and_unicode() {
        assert_eq!(url_encode("foo bar"), "foo%20bar");
        assert_eq!(url_encode("a+b"), "a%2Bb");
        assert_eq!(url_encode("天气"), "%E5%A4%A9%E6%B0%94");
    }

    #[test]
    fn defaults_match_frontend_presets() {
        let d = WebEngine::defaults();
        assert_eq!(d.len(), 3);
        // 必应优先，便于国内环境验证。
        assert_eq!(d[0].keyword, "bi");
        assert_eq!(d[1].keyword, "b");
        assert_eq!(d[2].keyword, "g");
        assert!(d[0].url_template.contains("{q}"));
        assert!(d[0].url_template.contains("bing.com"));
    }
}
