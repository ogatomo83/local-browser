//! 文書の中身を読み取る部分 (lb-extract)。
//!
//! ファイル操作・ネットワークを使わない純粋な Rust。バイト列を受け取って、
//! タイトル・見出し・本文・表示用 HTML を返すだけ。
//! - lb では wasm コンポーネント (extractor/、wasm32-wasip2) に包んで wasmtime で隔離して動かす
//! - ブラウザ (モード C) では wasm32-unknown-unknown にして Worker から呼ぶ

use pulldown_cmark::{CowStr, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub struct Heading {
    pub level: u8,
    pub text: String,
    pub anchor: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    pub title: String,
    pub headings: Vec<Heading>,
    /// 検索用の本文 (タグなし)
    pub body: String,
    /// 表示用の HTML (危険な要素は無害化済み)
    pub html: String,
}

pub const EXTENSIONS: &[&str] = &["md", "markdown", "txt", "html", "htm"];

pub fn extract(path: &str, content: &[u8]) -> Result<Document, String> {
    let text = String::from_utf8_lossy(content);
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "md" | "markdown" => Ok(markdown(path, &text)),
        "html" | "htm" => Ok(html(path, &text)),
        "txt" => Ok(plain(path, &text)),
        other => Err(format!("unsupported extension: {other}")),
    }
}

// ---------------------------------------------------------------- markdown

fn markdown(path: &str, src: &str) -> Document {
    let (front_title, src) = strip_front_matter(src);

    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_FOOTNOTES);
    opts.insert(Options::ENABLE_HEADING_ATTRIBUTES);

    let mut events: Vec<Event> = Parser::new_ext(src, opts).collect();

    // 1 パス目: 見出しテキストを集めてアンカーを決める
    let mut headings = Vec::new();
    let mut slugs: HashMap<String, usize> = HashMap::new();
    let mut i = 0;
    while i < events.len() {
        if let Event::Start(Tag::Heading { level, id, .. }) = &events[i] {
            let level = heading_level(*level);
            let explicit = id.as_ref().map(|s| s.to_string());
            let mut text = String::new();
            let mut j = i + 1;
            while j < events.len() {
                match &events[j] {
                    Event::End(TagEnd::Heading(_)) => break,
                    Event::Text(t) | Event::Code(t) => text.push_str(t),
                    _ => {}
                }
                j += 1;
            }
            let text = text.trim().to_string();
            let anchor = explicit.unwrap_or_else(|| unique_slug(&text, &mut slugs));
            if let Event::Start(Tag::Heading { id, .. }) = &mut events[i] {
                *id = Some(CowStr::from(anchor.clone()));
            }
            headings.push(Heading { level, text, anchor });
            i = j;
        }
        i += 1;
    }

    // 2 パス目: 本文テキスト抽出 + 安全化
    let mut body = String::new();
    let safe: Vec<Event> = events
        .into_iter()
        .map(|ev| {
            match &ev {
                Event::Text(t) | Event::Code(t) => body.push_str(t),
                Event::SoftBreak | Event::HardBreak => body.push(' '),
                Event::End(
                    TagEnd::Paragraph
                    | TagEnd::Heading(_)
                    | TagEnd::Item
                    | TagEnd::CodeBlock
                    | TagEnd::TableRow,
                ) => body.push('\n'),
                Event::End(TagEnd::TableCell) => body.push(' '),
                _ => {}
            }
            sanitize(ev)
        })
        .collect();

    let mut out = String::new();
    pulldown_cmark::html::push_html(&mut out, safe.into_iter());

    let title = front_title
        .or_else(|| {
            headings
                .iter()
                .find(|h| h.level == 1)
                .map(|h| h.text.clone())
        })
        .unwrap_or_else(|| file_stem(path));

    Document {
        title,
        headings,
        body: collapse_blank_lines(&body),
        html: out,
    }
}

/// 生の HTML はテキストとしてエスケープし、危険なスキームの URL を潰す
fn sanitize(ev: Event) -> Event {
    match ev {
        Event::Html(s) | Event::InlineHtml(s) => Event::Text(s),
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Link {
            link_type,
            dest_url: safe_url(dest_url),
            title,
            id,
        }),
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Image {
            link_type,
            dest_url: safe_url(dest_url),
            title,
            id,
        }),
        other => other,
    }
}

fn safe_url(url: CowStr) -> CowStr {
    let lower = url.trim().to_ascii_lowercase();
    let bad = ["javascript:", "vbscript:", "data:text", "file:"];
    if bad.iter().any(|b| lower.starts_with(b)) {
        CowStr::from("#")
    } else {
        url
    }
}

fn strip_front_matter(src: &str) -> (Option<String>, &str) {
    let Some(rest) = src.strip_prefix("---\n").or_else(|| src.strip_prefix("---\r\n")) else {
        return (None, src);
    };
    let Some(end) = rest.find("\n---") else {
        return (None, src);
    };
    let fm = &rest[..end];
    let after = &rest[end + 4..];
    let after = after.trim_start_matches(['\r', '\n']);
    let title = fm.lines().find_map(|l| {
        l.strip_prefix("title:")
            .map(|v| v.trim().trim_matches(['"', '\'']).to_string())
            .filter(|v| !v.is_empty())
    });
    (title, after)
}

fn heading_level(l: HeadingLevel) -> u8 {
    match l {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// 日本語などの非 ASCII 文字はそのまま残すスラッグ
fn unique_slug(text: &str, seen: &mut HashMap<String, usize>) -> String {
    let mut slug = String::new();
    for c in text.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() || c == '_' || c == '-' {
            slug.push(c);
        } else if c.is_whitespace() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-').to_string();
    let slug = if slug.is_empty() { "section".to_string() } else { slug };
    let n = seen.entry(slug.clone()).or_insert(0);
    *n += 1;
    if *n == 1 { slug } else { format!("{slug}-{}", *n - 1) }
}

// ---------------------------------------------------------------- html

fn html(path: &str, src: &str) -> Document {
    let title = tag_text(src, "title")
        .or_else(|| tag_text(src, "h1"))
        .unwrap_or_else(|| file_stem(path));
    Document {
        title,
        headings: Vec::new(),
        body: collapse_blank_lines(&strip_tags(src)),
        // HTML ファイルは原文のまま返す。表示側で sandbox iframe に入れる前提。
        html: src.to_string(),
    }
}

fn tag_text(src: &str, tag: &str) -> Option<String> {
    let lower = src.to_ascii_lowercase();
    let open = lower.find(&format!("<{tag}"))?;
    let start = open + lower[open..].find('>')? + 1;
    let end = start + lower[start..].find(&format!("</{tag}"))?;
    let t = strip_tags(&src[start..end]).trim().to_string();
    (!t.is_empty()).then_some(t)
}

fn strip_tags(src: &str) -> String {
    let lower = src.to_ascii_lowercase();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    let bytes = src.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'<' {
            // script / style は中身ごと捨てる
            let mut skipped = false;
            for t in ["script", "style"] {
                if lower[i + 1..].starts_with(t) {
                    if let Some(e) = lower[i..].find(&format!("</{t}")) {
                        let close = i + e;
                        i = close + lower[close..].find('>').map(|x| x + 1).unwrap_or(0);
                        skipped = true;
                    }
                }
            }
            if skipped {
                continue;
            }
            match src[i..].find('>') {
                Some(e) => {
                    i += e + 1;
                    out.push(' ');
                }
                None => break,
            }
        } else {
            let next = src[i..].find('<').map(|e| i + e).unwrap_or(src.len());
            out.push_str(&src[i..next]);
            i = next;
        }
    }
    out.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

// ---------------------------------------------------------------- plain text

fn plain(path: &str, src: &str) -> Document {
    let title = src
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.chars().take(80).collect())
        .unwrap_or_else(|| file_stem(path));
    Document {
        title,
        headings: Vec::new(),
        body: src.to_string(),
        html: format!("<pre>{}</pre>", escape(src)),
    }
}

// ---------------------------------------------------------------- utils

fn file_stem(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match name.rfind('.') {
        Some(i) if i > 0 => name[..i].to_string(),
        _ => name.to_string(),
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn collapse_blank_lines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut blank = 0;
    for line in s.lines() {
        let l = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if l.is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(&l);
        out.push('\n');
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_headings_and_title() {
        let d = extract("a/b.md", "---\ntitle: 手順\n---\n# 見出し\n## キャッシュ\n## キャッシュ\n本文".as_bytes()).unwrap();
        assert_eq!(d.title, "手順");
        let anchors: Vec<_> = d.headings.iter().map(|h| h.anchor.as_str()).collect();
        assert_eq!(anchors, ["見出し", "キャッシュ", "キャッシュ-1"]);
        assert!(d.body.contains("本文"));
    }

    #[test]
    fn neutralizes_dangerous_markup() {
        let d = extract("x.md", b"<script>alert(1)</script>\n\n[a](javascript:alert(1))").unwrap();
        assert!(!d.html.contains("<script>"));
        assert!(!d.html.contains("javascript:"));
    }

    #[test]
    fn unknown_extension() {
        assert!(extract("x.pdf", b"").is_err());
    }
}
