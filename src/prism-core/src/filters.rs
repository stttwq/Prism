//! G7b+G7c：broker 侧 stat 过滤（`size:` / `dm:` / `dc:` / `file:` / `folder:`）。
//!
//! G7c（2026-08-30）起判定下沉到索引器扫描内：VolumeIndex 每记录携带
//! size/mtime/ctime（serde skip 不入 sidecar，服务启动后由维护 tick 分块
//! 全树 stat 填充、USN 变更失效重填），`QueryFilters::stat_matches_record`
//! 在 Top-K 堆前执行——纯过滤查询（`size:>1mb` 无名字词）从此覆盖全索引。
//! 本模块保留：过滤值解析/校验、`StatFilterSet` 语义、扫描内与候选集后置
//! （历史注入行不经索引器，broker 侧仍逐条磁盘 stat）共用的单条判定
//! [`stat_passes`]。
//!
//! 日期边界用本地时区（GetLocalTime + GetTimeZoneInformation），与 Everything
//! 的「今天」一致；不引入 chrono——民用日历换算用 Howard Hinnant 的
//! days_from_civil 算法（纯整数，无查找表）。
//!
//! 语法（broker parse_query 负责 token 化，本模块负责校验与匹配）：
//! - `size:` `>=`/`<=`/`>`/`<`/`=`前缀（可省）+ 数值 + 可选单位 b/kb/mb/gb/tb
//!   （无单位 = 字节）；或 `N..M` 区间；或桶名 empty/tiny/small/medium/large/
//!   huge/gigantic。
//! - `dm:`/`dc:` 同样前缀 + `YYYY[MM[DD]]` 紧凑日期（无斜杠）；或 `d1..d2`
//!   区间；或 today/yesterday/thisweek/lastweek/thismonth/lastmonth/thisyear/
//!   lastyear。
//! - `file:`/`folder:` 旗标，不取值（冒号后文本照常解析为下一 token）。

use crate::indexer_ipc::SearchFilter;
use crate::ipc::SearchResult;
use crate::ipc::SearchResultKind;

/// broker 已知的查询过滤字段（has_query_filters 用）。**不含 exclude_path**——
/// 那是 G3 的作用域排除，不该把 apps/web 从结果里挤出去。
pub const QUERY_FILTER_FIELDS: &[&str] = &["ext", "path", "size", "dm", "dc", "file", "folder"];

/// 索引器不认识、必须在 broker 候选集上后置执行的过滤字段。
pub const STAT_FILTER_FIELDS: &[&str] = &["size", "dm", "dc", "file", "folder"];

pub fn is_known_filter_field(field: &str) -> bool {
    QUERY_FILTER_FIELDS.contains(&field)
}

pub fn is_stat_filter_field(field: &str) -> bool {
    STAT_FILTER_FIELDS.contains(&field)
}

// ── size ────────────────────────────────────────────────────────────────

/// 闭区间 [lo, hi]；hi = None 表示上不封顶。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SizeSpec {
    lo: u64,
    hi: Option<u64>,
}

impl SizeSpec {
    fn matches(&self, len: u64) -> bool {
        len >= self.lo && self.hi.map_or(true, |hi| len <= hi)
    }
}

const KB: f64 = 1024.0;

/// 单个 size 原子：`123`、`1.5mb`、`2gb`（无单位 = 字节）。
fn parse_size_atom(raw: &str) -> Option<u64> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let split = s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    if num.is_empty() || !num.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    let value: f64 = num.parse().ok()?;
    let mult = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "kb" => KB,
        "mb" => KB * KB,
        "gb" => KB * KB * KB,
        "tb" => KB * KB * KB * KB,
        _ => return None,
    };
    let bytes = value * mult;
    if !bytes.is_finite() || bytes < 0.0 {
        return None;
    }
    Some(bytes.round() as u64)
}

/// Everything 的 size 桶名（近似边界，够用且好记）。
fn parse_size_bucket(raw: &str) -> Option<SizeSpec> {
    let spec = match raw.trim().to_ascii_lowercase().as_str() {
        "empty" => SizeSpec { lo: 0, hi: Some(0) },
        "tiny" => SizeSpec {
            lo: 0,
            hi: Some(10 * 1024 - 1),
        },
        "small" => SizeSpec {
            lo: 10 * 1024,
            hi: Some(100 * 1024 - 1),
        },
        "medium" => SizeSpec {
            lo: 100 * 1024,
            hi: Some((1024.0 * KB) as u64 - 1),
        },
        "large" => SizeSpec {
            lo: 1024 * 1024,
            hi: Some(16 * 1024 * 1024 - 1),
        },
        "huge" => SizeSpec {
            lo: 16 * 1024 * 1024,
            hi: Some(1024 * 1024 * 1024 - 1),
        },
        "gigantic" => SizeSpec {
            lo: 1024 * 1024 * 1024,
            hi: None,
        },
        _ => return None,
    };
    Some(spec)
}

/// 解析 `size:` 过滤值。解析失败返回 None（parse_query 据此把 token 降级为
/// 普通文本——半截输入静默变"查不到"比当文本更糟）。
pub fn parse_size_spec(raw: &str) -> Option<SizeSpec> {
    let s = raw.trim();
    if let Some(rest) = s.strip_prefix(">=") {
        return parse_size_atom(rest).map(|v| SizeSpec { lo: v, hi: None });
    }
    if let Some(rest) = s.strip_prefix("<=") {
        return parse_size_atom(rest).map(|v| SizeSpec { lo: 0, hi: Some(v) });
    }
    if let Some(rest) = s.strip_prefix('>') {
        return parse_size_atom(rest).map(|v| SizeSpec {
            lo: v.saturating_add(1),
            hi: None,
        });
    }
    if let Some(rest) = s.strip_prefix('<') {
        return parse_size_atom(rest).map(|v| SizeSpec {
            lo: 0,
            hi: v.checked_sub(1),
        });
    }
    if let Some((a, b)) = s.split_once("..") {
        let lo = parse_size_atom(a)?;
        let hi = parse_size_atom(b)?;
        if lo > hi {
            return None;
        }
        return Some(SizeSpec { lo, hi: Some(hi) });
    }
    if let Some(v) = parse_size_atom(s) {
        return Some(SizeSpec { lo: v, hi: Some(v) });
    }
    parse_size_bucket(s)
}

/// parse_query 用的轻量校验（不需要 now）。
pub fn is_valid_size_value(raw: &str) -> bool {
    parse_size_spec(raw).is_some()
}

// ── date ────────────────────────────────────────────────────────────────

/// 本地墙钟「现在」。tz_offset = 本地 - UTC（东八区 +28800）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalNow {
    pub year: i64,
    pub month: i64,
    pub day: i64,
    /// 自 1970-01-01 起的本地天数（民用日历，非 UTC）。
    pub days: i64,
    pub secs_of_day: i64,
    pub tz_offset: i64,
}

impl LocalNow {
    /// 周一=0 .. 周日=6。1970-01-01 是周四（days=0 → 3）。
    pub fn day_of_week(&self) -> i64 {
        (self.days % 7 + 7 + 3) % 7
    }
    pub(crate) fn utc_epoch_of_day(&self, civil_day: i64) -> i64 {
        civil_day * 86400 - self.tz_offset
    }
}

/// Howard Hinnant days_from_civil：民用日期 → 自 1970-01-01 的天数。
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// 紧凑日期 `YYYY` / `YYYYMM` / `YYYYMMDD` → 本地 civil 天的 [start, end)。
fn parse_date_days(raw: &str) -> Option<(i64, i64)> {
    let s = raw.trim();
    if s.is_empty() || !s.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    match s.len() {
        4 => {
            let y = s.parse::<i64>().ok()?;
            Some((days_from_civil(y, 1, 1), days_from_civil(y + 1, 1, 1)))
        }
        6 => {
            let y = s[..4].parse::<i64>().ok()?;
            let m = s[4..].parse::<i64>().ok()?;
            if !(1..=12).contains(&m) {
                return None;
            }
            let end = if m == 12 {
                days_from_civil(y + 1, 1, 1)
            } else {
                days_from_civil(y, m + 1, 1)
            };
            Some((days_from_civil(y, m, 1), end))
        }
        8 => {
            let y = s[..4].parse::<i64>().ok()?;
            let m = s[4..6].parse::<i64>().ok()?;
            let d = s[6..].parse::<i64>().ok()?;
            if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
                return None;
            }
            Some((days_from_civil(y, m, d), days_from_civil(y, m, d) + 1))
        }
        _ => None,
    }
}

/// 命名日期 → 本地 civil 天 [start, end)。
fn parse_named_days(raw: &str, now: &LocalNow) -> Option<(i64, i64)> {
    const DATE_NAMED: &[&str] = &[
        "today",
        "yesterday",
        "thisweek",
        "lastweek",
        "thismonth",
        "lastmonth",
        "thisyear",
        "lastyear",
    ];
    let d = now.days;
    let dow = now.day_of_week();
    let (y, m) = (now.year, now.month);
    let this_month = days_from_civil(y, m, 1);
    let next_month = if m == 12 {
        days_from_civil(y + 1, 1, 1)
    } else {
        days_from_civil(y, m + 1, 1)
    };
    let last_month = if m == 1 {
        days_from_civil(y - 1, 12, 1)
    } else {
        days_from_civil(y, m - 1, 1)
    };
    Some(match raw.trim().to_ascii_lowercase().as_str() {
        "today" => (d, d + 1),
        "yesterday" => (d - 1, d),
        "thisweek" => (d - dow, d - dow + 7),
        "lastweek" => (d - dow - 7, d - dow),
        "thismonth" => (this_month, next_month),
        "lastmonth" => (last_month, this_month),
        "thisyear" => (days_from_civil(y, 1, 1), days_from_civil(y + 1, 1, 1)),
        "lastyear" => (days_from_civil(y - 1, 1, 1), days_from_civil(y, 1, 1)),
        _ => return None,
    })
}

/// 闭 UTC 秒区间 [lo, hi]；None = 不封。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DateSpec {
    lo: Option<i64>,
    hi: Option<i64>,
}

impl DateSpec {
    fn matches(&self, t: i64) -> bool {
        self.lo.map_or(true, |lo| t >= lo) && self.hi.map_or(true, |hi| t <= hi)
    }
    fn days(now: &LocalNow, start: i64, end: i64) -> Self {
        DateSpec {
            lo: Some(now.utc_epoch_of_day(start)),
            hi: Some(now.utc_epoch_of_day(end) - 1),
        }
    }
}

/// 解析 `dm:`/`dc:` 过滤值。
pub fn parse_date_spec(raw: &str, now: &LocalNow) -> Option<DateSpec> {
    let s = raw.trim();
    if let Some(rest) = s.strip_prefix(">=") {
        let (start, _) = date_days(rest, now)?;
        return Some(DateSpec {
            lo: Some(now.utc_epoch_of_day(start)),
            hi: None,
        });
    }
    if let Some(rest) = s.strip_prefix("<=") {
        let (_, end) = date_days(rest, now)?;
        return Some(DateSpec {
            lo: None,
            hi: Some(now.utc_epoch_of_day(end) - 1),
        });
    }
    if let Some(rest) = s.strip_prefix('>') {
        let (_, end) = date_days(rest, now)?;
        return Some(DateSpec {
            lo: Some(now.utc_epoch_of_day(end)),
            hi: None,
        });
    }
    if let Some(rest) = s.strip_prefix('<') {
        let (start, _) = date_days(rest, now)?;
        return Some(DateSpec {
            lo: None,
            hi: Some(now.utc_epoch_of_day(start) - 1),
        });
    }
    if let Some((a, b)) = s.split_once("..") {
        let (start, _) = date_days(a, now)?;
        let (_, end) = date_days(b, now)?;
        if start > end {
            return None;
        }
        return Some(DateSpec::days(now, start, end));
    }
    let (start, end) = date_days(s, now)?;
    Some(DateSpec::days(now, start, end))
}

fn date_days(raw: &str, now: &LocalNow) -> Option<(i64, i64)> {
    parse_date_days(raw).or_else(|| parse_named_days(raw, now))
}

/// parse_query 用的校验（全量语义校验，需要 now 解析命名/边界）。
pub fn is_valid_date_value(raw: &str, now: &LocalNow) -> bool {
    parse_date_spec(raw, now).is_some()
}

// ── 本地时间（Windows）──────────────────────────────────────────────────

#[cfg(windows)]
pub fn local_now() -> LocalNow {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    use windows::Win32::System::Time::GetTimeZoneInformation;
    // SAFETY：两个 API 均无特殊前置条件；SYSTEMTIME/TIME_ZONE_INFORMATION 是
    // 纯 POD 出参。Bias 语义：UTC = 本地 + Bias（分钟），故 offset = -Bias。
    let (st, bias_minutes) = unsafe {
        let st = GetLocalTime();
        let mut tz = windows::Win32::System::Time::TIME_ZONE_INFORMATION::default();
        let _ = GetTimeZoneInformation(&mut tz);
        (st, tz.Bias as i64)
    };
    let (y, m, d) = (st.wYear as i64, st.wMonth as i64, st.wDay as i64);
    LocalNow {
        year: y,
        month: m,
        day: d,
        days: days_from_civil(y, m, d),
        secs_of_day: (st.wHour as i64) * 3600 + (st.wMinute as i64) * 60 + st.wSecond as i64,
        tz_offset: -bias_minutes * 60,
    }
}

#[cfg(not(windows))]
pub fn local_now() -> LocalNow {
    // 非 Windows 构建（CI lint）退到 UTC 墙钟——日期语义退化但不影响编译。
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    LocalNow {
        year: 1970,
        month: 1,
        day: 1,
        days: secs / 86400,
        secs_of_day: secs % 86400,
        tz_offset: 0,
    }
}

// ── 候选集过滤 ──────────────────────────────────────────────────────────

/// 从请求过滤器收集 stat 类条件。日期 spec 在构建时用 now 解析一次，
/// 不逐候选重复解析。
pub fn stat_filter_set(filters: &[SearchFilter], now: &LocalNow) -> StatFilterSet {
    let mut set = StatFilterSet::default();
    for filter in filters {
        if !is_stat_filter_field(&filter.field) {
            continue;
        }
        match filter.field.as_str() {
            "size" => {
                if let Some(spec) = parse_size_spec(&filter.value) {
                    set.sizes.push(spec);
                }
            }
            "dm" => {
                if let Some(spec) = parse_date_spec(&filter.value, now) {
                    set.modified.push(spec);
                }
            }
            "dc" => {
                if let Some(spec) = parse_date_spec(&filter.value, now) {
                    set.created.push(spec);
                }
            }
            "file" => set.file_only = true,
            "folder" => set.folder_only = true,
            _ => {}
        }
    }
    set
}

#[derive(Default, Debug, Clone)]
pub struct StatFilterSet {
    sizes: Vec<SizeSpec>,
    modified: Vec<DateSpec>,
    created: Vec<DateSpec>,
    file_only: bool,
    folder_only: bool,
}

impl StatFilterSet {
    pub fn is_empty(&self) -> bool {
        !self.file_only
            && !self.folder_only
            && self.sizes.is_empty()
            && self.modified.is_empty()
            && self.created.is_empty()
    }
}

/// 逐候选磁盘 stat。调用方放 spawn_blocking——几十到几千次 stat 是磁盘 I/O。
/// stat 失败（文件消失/权限/挂载点掉线）= 候选失效，与 history 存在性复验
/// 同口径。size 检查跳过文件夹（文件夹大小无 Everything 语义）；日期对
/// 文件/文件夹都适用。
pub fn apply_stat_filters(mut items: Vec<SearchResult>, set: &StatFilterSet) -> Vec<SearchResult> {
    if set.is_empty() {
        return items;
    }
    items.retain(|item| passes(item, set));
    items
}

fn passes(item: &SearchResult, set: &StatFilterSet) -> bool {
    let is_folder = item.kind == SearchResultKind::Folder;
    // 纯旗标条件不碰磁盘 stat（原语义）；有 size/date 条件时 stat 失败=候选失效。
    if set.sizes.is_empty() && set.modified.is_empty() && set.created.is_empty() {
        return stat_passes(set, is_folder, None, None, None);
    }
    let Ok(meta) = std::fs::metadata(item.execute_id.as_ref()) else {
        return false;
    };
    stat_passes(
        set,
        is_folder,
        Some(meta.len()),
        system_time_epoch(meta.modified()),
        system_time_epoch(meta.created()),
    )
}

/// G7c：单条 stat 判定，索引器扫描内（元数据来自索引）与 broker 候选集后置
/// （元数据来自现场磁盘 stat）共用同一语义。任一 size 条件命中即过（OR），
/// dm/dc 同理；file/folder 旗标为 AND。size=None 表文件夹——size 条件跳过
/// 文件夹（文件夹大小无 Everything 语义），日期条件对文件夹照常生效。
/// size/mtime/ctime 传 None = 元数据未知（索引未填充/stat 失败）。
pub(crate) fn stat_passes(
    set: &StatFilterSet,
    is_directory: bool,
    size: Option<u64>,
    mtime: Option<i64>,
    ctime: Option<i64>,
) -> bool {
    if set.file_only && is_directory {
        return false;
    }
    if set.folder_only && !is_directory {
        return false;
    }
    if set.sizes.is_empty() && set.modified.is_empty() && set.created.is_empty() {
        return true;
    }
    let (Some(size), Some(mtime), Some(ctime)) = (size, mtime, ctime) else {
        return false;
    };
    if !set.sizes.is_empty() && !is_directory && !set.sizes.iter().any(|s| s.matches(size)) {
        return false;
    }
    if !set.modified.is_empty() && !set.modified.iter().any(|s| s.matches(mtime)) {
        return false;
    }
    if !set.created.is_empty() && !set.created.iter().any(|s| s.matches(ctime)) {
        return false;
    }
    true
}

fn system_time_epoch(t: std::io::Result<std::time::SystemTime>) -> Option<i64> {
    t.ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now_fixed() -> LocalNow {
        // 2024-03-15（周五）12:00 本地，tz +8h。
        LocalNow {
            year: 2024,
            month: 3,
            day: 15,
            days: days_from_civil(2024, 3, 15),
            secs_of_day: 12 * 3600,
            tz_offset: 8 * 3600,
        }
    }

    #[test]
    fn days_from_civil_known_values() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1970, 1, 2), 1);
        assert_eq!(days_from_civil(2024, 1, 1), 19723);
        // 闰年跨 2/29。
        assert_eq!(
            days_from_civil(2024, 3, 1) - days_from_civil(2024, 2, 1),
            29
        );
    }

    /// G7c：扫描内/后置共用的单条判定语义。
    #[test]
    fn g7c_stat_passes_shared_predicate() {
        let sf = |field: &str, value: &str| SearchFilter {
            field: field.into(),
            value: value.into(),
        };
        let now = now_fixed();
        let file_only = stat_filter_set(&[sf("file", "")], &now);
        assert!(stat_passes(&file_only, false, None, None, None));
        assert!(!stat_passes(&file_only, true, None, None, None));
        let folder_only = stat_filter_set(&[sf("folder", "")], &now);
        assert!(!stat_passes(&folder_only, false, None, None, None));

        // 元数据未知 = 不过 size/date 条件（索引未填充或 stat 失败同口径）。
        let size = stat_filter_set(&[sf("size", ">1b")], &now);
        assert!(!stat_passes(&size, false, None, None, None));
        assert!(stat_passes(&size, false, Some(10), Some(0), Some(0)));
        assert!(!stat_passes(&size, false, Some(0), Some(0), Some(0)));
        // size 条件跳过文件夹（文件夹大小无语义），其余条件照常。
        assert!(stat_passes(&size, true, Some(9_999_999), Some(0), Some(0)));

        // 多 size 条件 OR；dm 与 dc 各自独立。
        let multi = stat_filter_set(&[sf("size", ">1mb"), sf("size", "<200b")], &now);
        assert!(stat_passes(&multi, false, Some(150), Some(0), Some(0)));
        assert!(stat_passes(
            &multi,
            false,
            Some(2_000_000),
            Some(0),
            Some(0)
        ));
        assert!(!stat_passes(&multi, false, Some(5_000), Some(0), Some(0)));

        let modified = stat_filter_set(&[sf("dm", "20240315")], &now);
        let day_start = now.utc_epoch_of_day(now.days);
        assert!(stat_passes(
            &modified,
            false,
            Some(1),
            Some(day_start + 60),
            Some(0)
        ));
        assert!(!stat_passes(
            &modified,
            false,
            Some(1),
            Some(day_start - 60),
            Some(0)
        ));
    }

    #[test]
    fn day_of_week_monday_zero() {
        let now = now_fixed();
        // 2024-03-15 是周五。
        assert_eq!(now.day_of_week(), 4);
    }

    #[test]
    fn size_specs_parse_and_match() {
        let gt = parse_size_spec(">10mb").unwrap();
        assert!(gt.matches(10 * 1024 * 1024 + 1));
        assert!(!gt.matches(10 * 1024 * 1024));
        let range = parse_size_spec("1mb..100mb").unwrap();
        assert!(range.matches(1024 * 1024));
        assert!(!range.matches(999 * 1024));
        let exact = parse_size_spec("512").unwrap();
        assert!(exact.matches(512));
        assert!(!exact.matches(513));
        let le = parse_size_spec("<=2gb").unwrap();
        assert!(le.matches(2 * 1024 * 1024 * 1024));
        let frac = parse_size_spec("1.5mb").unwrap();
        assert!(frac.matches((1.5 * 1024.0 * 1024.0) as u64));
        assert!(parse_size_spec("gigantic")
            .unwrap()
            .matches(2 * 1024 * 1024 * 1024));
        assert!(parse_size_spec("abc").is_none());
        assert!(parse_size_spec("100mb..1mb").is_none());
        assert!(parse_size_spec("12xy").is_none());
    }

    #[test]
    fn date_specs_named_and_compact() {
        let now = now_fixed();
        // today = 本地 2024-03-15 00:00 .. 24:00（UTC 减 8h）。
        let today = parse_date_spec("today", &now).unwrap();
        let day_start = days_from_civil(2024, 3, 15) * 86400 - 8 * 3600;
        assert_eq!(today.lo, Some(day_start));
        assert_eq!(today.hi, Some(day_start + 86400 - 1));
        // 紧凑年月：202403。
        let month = parse_date_spec("202403", &now).unwrap();
        assert_eq!(
            month.lo,
            Some(days_from_civil(2024, 3, 1) * 86400 - 8 * 3600)
        );
        assert_eq!(
            month.hi,
            Some(days_from_civil(2024, 4, 1) * 86400 - 8 * 3600 - 1)
        );
        // 区间。
        let range = parse_date_spec("20240101..20240131", &now).unwrap();
        assert_eq!(
            range.lo,
            Some(days_from_civil(2024, 1, 1) * 86400 - 8 * 3600)
        );
        assert_eq!(
            range.hi,
            Some(days_from_civil(2024, 2, 1) * 86400 - 8 * 3600 - 1)
        );
        // 前缀：>2024 = 2025-01-01 起。
        let after = parse_date_spec(">2024", &now).unwrap();
        assert_eq!(
            after.lo,
            Some(days_from_civil(2025, 1, 1) * 86400 - 8 * 3600)
        );
        assert_eq!(after.hi, None);
        // 非法。
        assert!(parse_date_spec("202413", &now).is_none());
        assert!(parse_date_spec("nonsense", &now).is_none());
        assert!(!is_valid_date_value("202413", &now));
        assert!(is_valid_date_value("thisweek", &now));
        assert!(is_valid_date_value(">=20240101", &now));
    }

    #[test]
    fn stat_filter_set_collects_fields() {
        let now = now_fixed();
        let filters = vec![
            SearchFilter {
                field: "size".into(),
                value: ">1mb".into(),
            },
            SearchFilter {
                field: "file".into(),
                value: String::new(),
            },
            SearchFilter {
                field: "ext".into(),
                value: "pdf".into(),
            },
        ];
        let set = stat_filter_set(&filters, &now);
        assert!(!set.is_empty());
        assert!(set.file_only);
        assert_eq!(set.sizes.len(), 1);
        assert!(set.modified.is_empty());
    }

    #[test]
    fn apply_keeps_matching_file_and_drops_others() {
        fn file_result(path: &std::path::Path) -> SearchResult {
            SearchResult {
                kind: SearchResultKind::File,
                title: std::sync::Arc::from("t"),
                subtitle: std::sync::Arc::from(""),
                execute_id: std::sync::Arc::from(path.to_str().unwrap()),
                target: crate::shell::ActionTarget::new(
                    crate::shell::TargetKind::File,
                    path.to_str().unwrap().to_owned(),
                ),
                match_spans: Vec::new(),
                match_metadata: None,
            }
        }
        fn folder_result(path: &std::path::Path) -> SearchResult {
            SearchResult {
                kind: SearchResultKind::Folder,
                title: std::sync::Arc::from("t"),
                subtitle: std::sync::Arc::from(""),
                execute_id: std::sync::Arc::from(path.to_str().unwrap()),
                target: crate::shell::ActionTarget::new(
                    crate::shell::TargetKind::Directory,
                    path.to_str().unwrap().to_owned(),
                ),
                match_spans: Vec::new(),
                match_metadata: None,
            }
        }

        let dir = std::env::temp_dir().join(format!("prism-filters-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let big = dir.join("big.dat");
        let small = dir.join("small.txt");
        std::fs::write(&big, vec![0u8; 2048]).unwrap();
        std::fs::write(&small, b"hi").unwrap();

        let now = local_now();
        let set = stat_filter_set(
            &[SearchFilter {
                field: "size".into(),
                value: ">1kb".into(),
            }],
            &now,
        );
        let items = vec![file_result(&big), file_result(&small)];
        let kept = apply_stat_filters(items, &set);
        assert_eq!(kept.len(), 1);
        assert!(kept[0].execute_id.ends_with("big.dat"));

        // dm:today 应命中刚写入的文件。
        let set = stat_filter_set(
            &[SearchFilter {
                field: "dm".into(),
                value: "today".into(),
            }],
            &now,
        );
        let items = vec![file_result(&big)];
        assert_eq!(apply_stat_filters(items, &set).len(), 1);

        // folder_only 保留目录行、丢文件行（无需 stat）。
        let set = stat_filter_set(
            &[SearchFilter {
                field: "folder".into(),
                value: String::new(),
            }],
            &now,
        );
        let items = vec![folder_result(&dir), file_result(&big)];
        let kept = apply_stat_filters(items, &set);
        assert_eq!(kept.len(), 1);
        assert!(kept[0]
            .execute_id
            .ends_with(dir.file_name().unwrap().to_str().unwrap()));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
