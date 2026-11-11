//! クエリを語に分け、語ごとに「そのまま / 自動で直す / もしかして / 分割」を決める。
//!
//! 規則 (設計 v2 で合意したもの):
//! - 語の一覧にある語 (df > 0) は直さない
//! - かな: まずひらがな ↔ カタカナを入れ替えて一覧にあれば自動 (ろーるばっく → ロールバック)
//! - 英数字・かな: 8 文字未満は距離 1、8 文字以上は距離 2 まで。
//!   1 位の候補の df が 2 位の 3 倍以上なら自動、そうでなければ「もしかして」
//! - 2 文字の漢字語: 1 文字違いの候補のうち、違う文字どうしの音読みが同じものだけ自動。
//!   読みが違う候補しかなければ「もしかして」 (検索 の隣には 検討 と 検証 がいる)
//! - 3 文字以上の漢字語で一覧にないもの: 2 文字ずつに分けて、それぞれ上の規則で扱う
//! - 3 文字以上の漢字語が「書」「者」などの接尾辞で終わるなら、語幹で探す
//!   (「デプロイ手順書」で「デプロイ手順」の文書を出す。英語の stemming と同じ考え方)
//! - 短すぎる語 (英数字 4 文字未満、かな 3 文字未満、1 文字) と数字だけの語は直さない

use crate::normalize::{normalize, swap_kana};
use crate::readings::same_onyomi;
use crate::tokenize::{Script, bigrams, tokens};
use crate::vocab::Vocab;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Fix {
    /// 直さない
    Keep,
    /// 自動で直す
    Auto,
    /// 直さず「もしかして」で提案する
    Suggest,
    /// 一覧にない漢字の複合語を 2 文字ずつに分けた (見た目は変えない)
    Split,
}

#[derive(Debug, Clone, Serialize)]
pub struct TermPlan {
    /// 正規化した語 (クエリのまま)
    pub term: String,
    pub df: u64,
    pub fix: Fix,
    /// 置き換える語。Keep なら [term]
    pub to: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub terms: Vec<TermPlan>,
}

impl Plan {
    /// 最初に試す語 (分割だけは最初から使う)
    pub fn exact(&self) -> Vec<String> {
        self.collect(|f| f == Fix::Split)
    }

    /// 自動補正を当てた語。自動補正がなければ None
    pub fn corrected(&self) -> Option<Vec<String>> {
        self.has(Fix::Auto).then(|| self.collect(|f| matches!(f, Fix::Split | Fix::Auto)))
    }

    /// 「もしかして」の語 (自動補正も含む)。提案がなければ None
    pub fn suggestion(&self) -> Option<Vec<String>> {
        self.has(Fix::Suggest).then(|| self.collect(|f| f != Fix::Keep))
    }

    /// 自動補正で変わった (元の語, 直した語)
    pub fn changes(&self, fix: Fix) -> Vec<(String, String)> {
        self.terms
            .iter()
            .filter(|t| t.fix == fix)
            .map(|t| (t.term.clone(), t.to.join("")))
            .collect()
    }

    fn has(&self, f: Fix) -> bool {
        self.terms.iter().any(|t| t.fix == f)
    }

    fn collect(&self, apply: impl Fn(Fix) -> bool) -> Vec<String> {
        let mut out = Vec::new();
        for t in &self.terms {
            if apply(t.fix) {
                out.extend(t.to.iter().cloned());
            } else {
                out.push(t.term.clone());
            }
        }
        out
    }
}

pub fn plan(q: &str, vocab: &Vocab) -> Plan {
    let norm = normalize(q);
    let terms = tokens(&norm)
        .into_iter()
        .map(|t| plan_term(&t.text, t.script, t.len, vocab))
        .collect();
    Plan { terms }
}

fn keep(term: &str, df: u64) -> TermPlan {
    TermPlan { term: term.to_string(), df, fix: Fix::Keep, to: vec![term.to_string()] }
}

/// 落としてよい接尾辞。語幹が 2 文字以上残るときだけ落とす
const SUFFIXES: &[char] = &['書', '者', '化', '性', '的', '類', '表', '集', '案', '版', '等', '用'];

fn plan_term(term: &str, script: Script, len: usize, v: &Vocab) -> TermPlan {
    let df = v.df(term);
    if script == Script::Kanji && len >= 3 && term.ends_with(SUFFIXES) {
        let stem: String = term.chars().take(len - 1).collect();
        if v.df(&stem) > 0 {
            return TermPlan { term: term.into(), df, fix: Fix::Split, to: vec![stem] };
        }
    }
    if df > 0 || v.is_empty() || len < 2 {
        return keep(term, df);
    }
    match script {
        Script::Kanji if len == 2 => {
            let (to, fix) = fix_kanji2(term, v);
            match fix {
                Fix::Keep => keep(term, df),
                f => TermPlan { term: term.into(), df, fix: f, to: vec![to] },
            }
        }
        Script::Kanji => {
            // 一覧にない複合語は 2 文字ずつに分ける。分けた断片に一覧にないものがあれば、それぞれ直す。
            // 語の境目をまたぐ断片 (検索手順 の 索手) は直せなくて当然なので捨てる。
            // ただし、どの断片にも入らない文字が出たら分けるのをやめる
            let mut parts = Vec::new();
            let mut covered = vec![false; len];
            let mut worst = Fix::Split;
            for (i, b) in bigrams(term).into_iter().enumerate() {
                let to = if v.df(&b) > 0 {
                    b
                } else {
                    let (to, fix) = fix_kanji2(&b, v);
                    match fix {
                        Fix::Keep => continue,
                        Fix::Suggest => worst = Fix::Suggest,
                        Fix::Auto if worst == Fix::Split => worst = Fix::Auto,
                        _ => {}
                    }
                    to
                };
                covered[i] = true;
                covered[i + 1] = true;
                parts.push(to);
            }
            if parts.is_empty() || covered.contains(&false) {
                return keep(term, df);
            }
            TermPlan { term: term.into(), df, fix: worst, to: parts }
        }
        Script::Latin | Script::Hiragana | Script::Katakana => {
            let kana = script != Script::Latin;
            // かなの書き分け (ろーるばっく / ロールバック)。長音の規則をかけ直すため normalize を通す
            let swapped = if kana { normalize(&swap_kana(term)) } else { String::new() };
            if kana && v.df(&swapped) > 0 {
                return TermPlan { term: term.into(), df, fix: Fix::Auto, to: vec![swapped] };
            }
            let min = if kana { 3 } else { 4 };
            if len < min || term.chars().all(|c| c.is_ascii_digit()) {
                return keep(term, df);
            }
            let max = if len >= 8 { 2 } else { 1 };
            let mut cands = v.candidates(term, max);
            if kana {
                cands.extend(v.candidates(&swapped, max));
                cands.sort_by(|a, b| a.dist.cmp(&b.dist).then(b.df.cmp(&a.df)).then(a.term.cmp(&b.term)));
                cands.dedup_by(|a, b| a.term == b.term);
            }
            let Some(top) = cands.first() else { return keep(term, df) };
            // 同じ距離の 2 位と比べて、十分に差があるときだけ自動
            let runner = cands.iter().skip(1).find(|c| c.dist == top.dist).map(|c| c.df).unwrap_or(0);
            let fix = if top.df >= runner.saturating_mul(3).max(1) { Fix::Auto } else { Fix::Suggest };
            TermPlan { term: term.into(), df, fix, to: vec![top.term.clone()] }
        }
    }
}

/// 2 文字の漢字語の補正。1 文字だけ違う 2 文字語を探す。
fn fix_kanji2(term: &str, v: &Vocab) -> (String, Fix) {
    let tc: Vec<char> = term.chars().collect();
    let cands: Vec<_> = v
        .candidates(term, 1)
        .into_iter()
        .filter(|c| c.term.chars().count() == 2)
        .collect();
    let reading_ok = |c: &str| {
        c.chars().zip(tc.iter()).all(|(a, &b)| a == b || same_onyomi(a, b))
    };
    // 読みが同じ候補を優先し、その中で df が多いもの
    if let Some(c) = cands.iter().filter(|c| reading_ok(&c.term)).max_by_key(|c| c.df) {
        return (c.term.clone(), Fix::Auto);
    }
    match cands.iter().max_by_key(|c| c.df) {
        Some(c) => (c.term.clone(), Fix::Suggest),
        None => (term.to_string(), Fix::Keep),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vocab() -> Vocab {
        Vocab::from_unsorted(
            [
                ("k8s", 6), ("kubernetes", 4), ("postgres", 5), ("ロールバック", 3), ("サーバ", 8),
                ("検索", 9), ("検討", 7), ("検証", 6), ("障害", 5), ("経費", 3), ("承認", 4),
                ("当番", 2), ("手順", 5), ("順書", 1), ("デプロイ", 6), ("redux", 2), ("redis", 2),
            ]
            .map(|(k, v)| (k.to_string(), v)),
        )
    }

    fn fix(q: &str) -> (Fix, String) {
        let p = plan(q, &vocab());
        let t = &p.terms[0];
        (t.fix, t.to.join(" "))
    }

    #[test]
    fn typos() {
        assert_eq!(fix("k8se"), (Fix::Auto, "k8s".into()));
        assert_eq!(fix("kubernets"), (Fix::Auto, "kubernetes".into()));
        assert_eq!(fix("postgress"), (Fix::Auto, "postgres".into()));
        assert_eq!(fix("ロールバク"), (Fix::Auto, "ロールバック".into()));
        assert_eq!(fix("ろーるばっく"), (Fix::Auto, "ロールバック".into()));
        assert_eq!(fix("ろーるばく"), (Fix::Auto, "ロールバック".into()));
        assert_eq!(fix("さーばー"), (Fix::Auto, "サーバ".into()));
        assert_eq!(fix("ロールバック"), (Fix::Keep, "ロールバック".into()));
    }

    #[test]
    fn kanji_needs_same_reading() {
        assert_eq!(fix("検策"), (Fix::Auto, "検索".into()));
        assert_eq!(fix("障碍"), (Fix::Auto, "障害".into()));
        assert_eq!(fix("経比"), (Fix::Auto, "経費".into()));
        assert_eq!(fix("承任"), (Fix::Auto, "承認".into()));
        assert_eq!(fix("当晩"), (Fix::Auto, "当番".into()));
        // 検査 → 検索/検討/検証: 読みが合わないので自動では直さない
        assert_eq!(fix("検査").0, Fix::Suggest);
    }

    #[test]
    fn compound_split() {
        let p = plan("デプロイ手順書", &vocab());
        assert_eq!(p.exact(), ["デプロイ", "手順"]);
        assert!(p.corrected().is_none());
        let p = plan("検索手順", &vocab());
        assert_eq!(p.exact(), ["検索", "手順"]);
    }

    #[test]
    fn ambiguous_is_suggestion() {
        // redix は redux と redis の両方から距離 1、df も同じ → 自動にしない
        assert_eq!(fix("redix").0, Fix::Suggest);
    }

    #[test]
    fn short_words_untouched() {
        assert_eq!(fix("k9s").0, Fix::Keep);
        assert_eq!(fix("2026").0, Fix::Keep);
    }
}
