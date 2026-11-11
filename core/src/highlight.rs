//! 元の文書の文字列の上で、正規化済みの語の位置を探す。
//!
//! 「k8s」「サーバ」で探しても、画面には元の「K8s」「サーバー」を <mark> で囲んで見せたい。
//! そのために normalize_map の対応表で、正規化後の位置を元の位置に戻す。

use crate::normalize::{Normalized, normalize_map};
use crate::tokenize::script;

/// 元の文字列の中で、語が現れる範囲 (文字単位)。重なりはまとめる。
pub fn find_ranges(n: &Normalized, terms: &[String]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for t in terms {
        let tc: Vec<char> = t.chars().collect();
        if tc.is_empty() || tc.len() > n.chars.len() {
            continue;
        }
        let mut i = 0;
        while i + tc.len() <= n.chars.len() {
            if n.chars[i..i + tc.len()] == tc[..] {
                out.push((n.src_start[i], n.src_end[i + tc.len() - 1]));
                i += tc.len();
            } else {
                i += 1;
            }
        }
    }
    out.sort();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (s, e) in out {
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    merged
}

/// 語の元の表記 (「k8s」→「K8s」)。語の切れ目に一致する出現を優先する
/// (「postgres」を「PostgreSQL」の途中から取って「PostgreS」にしないため)
pub fn surface(text: &str, term: &str) -> Option<String> {
    surface_impl(text, term, false)
}

/// surface と同じだが、語の切れ目に一致する出現だけを見る
pub fn surface_bounded(text: &str, term: &str) -> Option<String> {
    surface_impl(text, term, true)
}

fn surface_impl(text: &str, term: &str, bounded_only: bool) -> Option<String> {
    let n = normalize_map(text);
    let tc: Vec<char> = term.chars().collect();
    if tc.is_empty() || tc.len() > n.chars.len() {
        return None;
    }
    let sc = |i: usize| script(n.chars[i]);
    let mut first = None;
    for i in 0..=n.chars.len() - tc.len() {
        if n.chars[i..i + tc.len()] != tc[..] {
            continue;
        }
        let end = i + tc.len();
        let range = (n.src_start[i], n.src_end[end - 1]);
        let bounded = (i == 0 || sc(i - 1) != sc(i)) && (end == n.chars.len() || sc(end) != sc(end - 1));
        if bounded {
            return Some(text.chars().skip(range.0).take(range.1 - range.0).collect());
        }
        first.get_or_insert(range);
    }
    let (s, e) = first.filter(|_| !bounded_only)?;
    Some(text.chars().skip(s).take(e - s).collect())
}

/// 検索結果の抜粋 (HTML)。<mark> 以外はエスケープ済み。空白は 1 つにまとめる。
pub fn snippet_html(text: &str, terms: &[String], width: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let n = normalize_map(text);
    let ranges = find_ranges(&n, terms);
    // 異なる語がいちばん多く入る窓を選ぶ (先頭から順に試す)
    let lead = width / 4;
    let start = ranges
        .iter()
        .map(|&(s, _)| s.saturating_sub(lead))
        .max_by_key(|&st| {
            let end = st + width;
            let inside = ranges.iter().filter(|&&(s, e)| s >= st && e <= end).count();
            (inside, std::cmp::Reverse(st))
        })
        .unwrap_or(0);
    let end = (start + width).min(chars.len());

    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    let mut in_mark = false;
    let mut last_space = false;
    for (i, &c) in chars.iter().enumerate().take(end).skip(start) {
        let open = ranges.iter().any(|&(s, _)| s == i);
        let close = ranges.iter().any(|&(_, e)| e == i);
        if close && in_mark {
            out.push_str("</mark>");
            in_mark = false;
        }
        if open && !in_mark {
            out.push_str("<mark>");
            in_mark = true;
        }
        if c.is_whitespace() {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
            continue;
        }
        last_space = false;
        push_escaped(&mut out, c);
    }
    if in_mark {
        out.push_str("</mark>");
    }
    if end < chars.len() {
        out.push('…');
    }
    out
}

fn push_escaped(out: &mut String, c: char) {
    match c {
        '&' => out.push_str("&amp;"),
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        '"' => out.push_str("&quot;"),
        '\'' => out.push_str("&#39;"),
        _ => out.push(c),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_original_form() {
        let s = snippet_html("手順: ＳＥＲＶＥＲは <b>慎重に</b>", &["server".into()], 100);
        assert_eq!(s, "手順: <mark>ＳＥＲＶＥＲ</mark>は &lt;b&gt;慎重に&lt;/b&gt;");
        assert_eq!(surface("Kubernetes (K8s) を使う", "k8s").as_deref(), Some("K8s"));
        assert_eq!(surface("サーバーの設定", "サーバ").as_deref(), Some("サーバ"));
        assert_eq!(surface("PostgreSQL と Postgres", "postgres").as_deref(), Some("Postgres"));
    }

    #[test]
    fn window_and_ellipsis() {
        let text = format!("{}k8s{}", "あ".repeat(100), "い".repeat(100));
        let s = snippet_html(&text, &["k8s".into()], 40);
        assert!(s.starts_with('…') && s.ends_with('…') && s.contains("<mark>k8s</mark>"));
    }
}
