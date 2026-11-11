//! 漢字の音読み (Unihan の kJapaneseOn から生成した data/onyomi.tsv)。
//!
//! 2 文字の漢字語を自動で補正してよいかの判定に使う。
//! 検策 → 検索 は「策」と「索」がどちらも「サク」なので自動で直す。
//! 検索の隣に住んでいる「検討」「検証」へは、読みが違うので勝手に直さない。
//!
//! Unicode Character Database (Unihan) © Unicode, Inc. — Unicode License v3

use std::collections::HashMap;
use std::sync::OnceLock;

static DATA: &str = include_str!("../data/onyomi.tsv");

fn table() -> &'static HashMap<char, Vec<&'static str>> {
    static T: OnceLock<HashMap<char, Vec<&'static str>>> = OnceLock::new();
    T.get_or_init(|| {
        DATA.lines()
            .filter_map(|l| {
                let (c, r) = l.split_once('\t')?;
                Some((c.chars().next()?, r.split(',').collect()))
            })
            .collect()
    })
}

pub fn onyomi(c: char) -> &'static [&'static str] {
    table().get(&c).map(|v| v.as_slice()).unwrap_or(&[])
}

/// 2 つの漢字が音読みを 1 つでも共有するか
pub fn same_onyomi(a: char, b: char) -> bool {
    let ra = onyomi(a);
    let rb = onyomi(b);
    ra.iter().any(|x| rb.contains(x))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readings() {
        assert!(same_onyomi('策', '索'));
        assert!(same_onyomi('碍', '害'));
        assert!(same_onyomi('比', '費'));
        assert!(same_onyomi('任', '認'));
        assert!(same_onyomi('晩', '番'));
        assert!(!same_onyomi('索', '討'));
        assert!(!same_onyomi('索', '証'));
    }
}
