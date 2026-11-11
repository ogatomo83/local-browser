//! 日本語向けの正規化。
//!
//! - NFKC (全角英数 → 半角、半角カナ → 全角 など)
//! - 小文字化
//! - 異体字の一部を揃える (髙 → 高)
//! - カタカナ語の末尾の長音を落とす (サーバー = サーバ)
//!
//! ひらがなとカタカナはここでは揃えない。揃えると「サーバーの」のように
//! カタカナ語と助詞の切れ目が消えてしまうため。かなの揺れ (ろーるばっく) は
//! クエリ側で、語の一覧にないときに変換して探し直す (query.rs)。
//!
//! 規則を変えたら NORM_VERSION を上げる。索引は版が違えば作り直される。

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::canonical_combining_class;

pub const NORM_VERSION: u32 = 1;

/// 正規化後の文字列と、各文字が元の文字列のどこから来たか (文字単位の位置)
pub struct Normalized {
    pub chars: Vec<char>,
    /// chars[i] の元になった文字の範囲 [src_start[i], src_end[i])
    pub src_start: Vec<usize>,
    pub src_end: Vec<usize>,
}

pub fn normalize(s: &str) -> String {
    normalize_map(s).chars.into_iter().collect()
}

pub fn normalize_map(s: &str) -> Normalized {
    let orig: Vec<char> = s.chars().collect();
    let mut chars = Vec::with_capacity(orig.len());
    let mut src_start = Vec::with_capacity(orig.len());
    let mut src_end = Vec::with_capacity(orig.len());
    let mut buf = String::new();
    let mut i = 0;
    while i < orig.len() {
        // 基底文字 + 後続の結合文字 (濁点など) をまとめて NFKC にかける
        let start = i;
        buf.clear();
        buf.push(orig[i]);
        i += 1;
        while i < orig.len() && is_combining(orig[i]) {
            buf.push(orig[i]);
            i += 1;
        }
        for c in buf.nfkc() {
            for c in c.to_lowercase() {
                chars.push(fold(c));
                src_start.push(start);
                src_end.push(i);
            }
        }
    }
    let mut n = Normalized { chars, src_start, src_end };
    strip_trailing_long_vowels(&mut n);
    n
}

fn is_combining(c: char) -> bool {
    // 半角カナの濁点・半濁点は結合文字ではないが、NFKC で前の文字と合成させたい
    matches!(c, '\u{FF9E}' | '\u{FF9F}') || canonical_combining_class(c) != 0
}

fn fold(c: char) -> char {
    itaiji(c)
}

/// ひらがな ↔ カタカナ を入れ替える (長音はそのまま)
pub fn swap_kana(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'ぁ'..='ゖ' | 'ゝ' | 'ゞ' => char::from_u32(c as u32 + 0x60).unwrap_or(c),
            'ァ'..='ヶ' | 'ヽ' | 'ヾ' => char::from_u32(c as u32 - 0x60).unwrap_or(c),
            _ => c,
        })
        .collect()
}

/// 異体字のうち、人名・地名でよく揺れるものだけ
fn itaiji(c: char) -> char {
    match c {
        '髙' => '高',
        '﨑' | '嵜' => '崎',
        '德' => '徳',
        '邊' | '邉' => '辺',
        '齋' | '齊' => '斉',
        '濱' => '浜',
        '澤' => '沢',
        '櫻' => '桜',
        '國' => '国',
        '廣' => '広',
        '眞' => '真',
        '藪' => '薮',
        '槇' => '槙',
        '冨' => '富',
        '惠' => '恵',
        '條' => '条',
        '龍' => '竜',
        _ => c,
    }
}

pub fn is_hiragana(c: char) -> bool {
    matches!(c, 'ぁ'..='ゖ' | 'ゝ' | 'ゞ')
}

pub fn is_katakana(c: char) -> bool {
    matches!(c, 'ァ'..='ヺ' | 'ヽ' | 'ヾ' | 'ー')
}

/// 3 文字以上のカタカナの並びが「ー」で終わるなら、その「ー」を落とす (サーバー → サーバ)
fn strip_trailing_long_vowels(n: &mut Normalized) {
    let len = n.chars.len();
    let mut keep = vec![true; len];
    let mut i = 0;
    while i < len {
        if !is_katakana(n.chars[i]) || n.chars[i] == 'ー' {
            i += 1;
            continue;
        }
        let start = i;
        while i < len && is_katakana(n.chars[i]) {
            i += 1;
        }
        let mut end = i;
        while end > start && n.chars[end - 1] == 'ー' {
            end -= 1;
        }
        if end - start >= 2 && end < i {
            for k in keep.iter_mut().take(i).skip(end) {
                *k = false;
            }
        }
    }
    if keep.iter().all(|&k| k) {
        return;
    }
    let mut out = Normalized { chars: Vec::new(), src_start: Vec::new(), src_end: Vec::new() };
    for (idx, k) in keep.iter().enumerate() {
        if *k {
            out.chars.push(n.chars[idx]);
            out.src_start.push(n.src_start[idx]);
            out.src_end.push(n.src_end[idx]);
        }
    }
    *n = out;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basics() {
        assert_eq!(normalize("ｶﾞｲﾄﾞ"), "ガイド");
        assert_eq!(normalize("Ｋ８Ｓ"), "k8s");
        assert_eq!(normalize("サーバー"), "サーバ");
        assert_eq!(normalize("サーバーの設定"), "サーバの設定");
        assert_eq!(normalize("ルーター設定"), "ルータ設定");
        assert_eq!(normalize("髙橋"), "高橋");
        // 1 文字 + 長音、ひらがなの長音は落とさない
        assert_eq!(normalize("ゲー"), "ゲー");
        assert_eq!(normalize("すごーい"), "すごーい");
        assert_eq!(swap_kana("ろーるばっく"), "ロールバック");
    }

    #[test]
    fn map_points_to_original() {
        let n = normalize_map("ｶﾞｲﾄﾞ");
        assert_eq!(n.chars, vec!['ガ', 'イ', 'ド']);
        assert_eq!((n.src_start[0], n.src_end[0]), (0, 2));
        assert_eq!((n.src_start[2], n.src_end[2]), (3, 5));
    }
}
