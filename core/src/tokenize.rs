//! 正規化済みの文字列を、字種の切れ目で語に分ける。
//!
//! 英数字の並び / ひらがなの並び / カタカナの並び / 漢字の並び がそれぞれ 1 語。
//! 「ー」は直前の文字の字種に付ける。
//! 漢字の並びは長い複合語になりやすいので、語の一覧には 2 文字ずつの断片 (bigram) も入れる。

use crate::normalize::{is_hiragana, is_katakana};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Script {
    Latin,
    Hiragana,
    Katakana,
    Kanji,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub text: String,
    pub script: Script,
    pub len: usize,
    /// 正規化済み文字列の中での開始位置 (文字単位)
    pub start: usize,
}

pub fn is_kanji(c: char) -> bool {
    matches!(c,
        '\u{3400}'..='\u{4DBF}' | '\u{4E00}'..='\u{9FFF}' | '\u{F900}'..='\u{FAFF}'
        | '\u{20000}'..='\u{3FFFF}' | '々' | '〆')
}

pub fn script(c: char) -> Option<Script> {
    if is_hiragana(c) {
        Some(Script::Hiragana)
    } else if is_katakana(c) {
        Some(Script::Katakana)
    } else if is_kanji(c) {
        Some(Script::Kanji)
    } else if c.is_alphanumeric() || c == '_' {
        Some(Script::Latin)
    } else {
        None
    }
}

/// 正規化済みの文字列を語に分ける
pub fn tokens(norm: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_script: Option<Script> = None;
    let mut len = 0;
    let mut start = 0;
    for (i, c) in norm.chars().enumerate() {
        let s = match (c, cur_script) {
            ('ー', Some(Script::Hiragana | Script::Katakana)) => cur_script,
            _ => script(c),
        };
        if s != cur_script && !cur.is_empty() {
            out.push(Token { text: std::mem::take(&mut cur), script: cur_script.unwrap(), len, start });
            len = 0;
        }
        cur_script = s;
        if s.is_some() {
            if cur.is_empty() {
                start = i;
            }
            cur.push(c);
            len += 1;
        }
    }
    if let (false, Some(s)) = (cur.is_empty(), cur_script) {
        out.push(Token { text: cur, script: s, len, start });
    }
    // 「ー」だけの語は捨てる
    out.retain(|t| t.text.chars().any(|c| c != 'ー'));
    out
}

pub fn bigrams(s: &str) -> Vec<String> {
    let cs: Vec<char> = s.chars().collect();
    cs.windows(2).map(|w| w.iter().collect()).collect()
}

/// 語の一覧 (補正の辞書) に入れる語。長すぎる語は入れない。
pub fn index_terms(norm: &str) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    for t in tokens(norm) {
        if t.len > 40 {
            continue;
        }
        if t.script == Script::Kanji && t.len >= 3 {
            set.extend(bigrams(&t.text));
        }
        set.insert(t.text);
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize;

    #[test]
    fn split_by_script() {
        let ts: Vec<String> = tokens(&normalize("デプロイ手順書 k8s, Cookie認証です"))
            .into_iter()
            .map(|t| t.text)
            .collect();
        assert_eq!(ts, ["デプロイ", "手順書", "k8s", "cookie", "認証", "です"]);
        let ts: Vec<String> = tokens(&normalize("サーバーの設定")).into_iter().map(|t| t.text).collect();
        assert_eq!(ts, ["サーバ", "の", "設定"]);
    }

    #[test]
    fn kanji_bigrams() {
        let t = index_terms(&normalize("障害対応"));
        assert!(t.contains("障害対応") && t.contains("障害") && t.contains("害対") && t.contains("対応"));
    }
}
