// 验证拼音匹配：搜索 "zhihu" 是否同时匹配 "知乎" 和 "之火"
use prism_core::pinyin;

fn main() {
    // 模拟用户输入拼音
    for query in &["zhihu", "zhi", "zh", "知乎"] {
        println!("=== Query: {} ===", query);
        for name in &["知乎", "之火", "心之火", "知乎日报"] {
            if let Some(m) = pinyin::match_name(name, query) {
                println!(
                    "  MATCH: {} -> kind={:?} class={} pos={}",
                    name, m.kind, m.class, m.position
                );
            } else {
                println!("  no match: {}", name);
            }
        }
        println!();
    }
}
