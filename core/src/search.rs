//! 段階的な検索 (lb とブラウザで共通)。
//!
//! 1. そのままの語で AND (一覧にない漢字の複合語だけは 2 文字ずつに分ける)
//! 2. 0 件なら、自動補正した語で AND → 画面に「k8s の結果を表示しています」
//! 3. それでも 0 件なら、語の一部が一致する文書 → 「近い文書」
//!
//! どの段階でも、提案 (もしかして) があれば一緒に返す。
//! exact = true なら 1 だけ (「元の語のまま検索」)。
//! 文書を引く部分 ([`Retriever`]) は呼ぶ側が用意する (SQLite FTS5 / 自前の索引)。

use crate::highlight::{surface, surface_bounded};
use crate::index::{Index, Match, Scored};
use crate::{Fix, Plan, Vocab};
use serde::Serialize;

/// 抜粋と表記の復元に使う文字列
pub trait Text {
    fn title(&self) -> &str;
    fn body(&self) -> &str;
}

pub trait Retriever {
    type Row: Text;
    type Error;
    /// すべての語を含む文書
    fn find_all(&self, terms: &[String], limit: usize, offset: usize) -> Result<(Vec<Self::Row>, usize), Self::Error>;
    /// 語の一部が一致する文書 (最後の安全網)
    fn find_loose(&self, terms: &[String], limit: usize, offset: usize) -> Result<(Vec<Self::Row>, usize), Self::Error>;
}

/// 自前の索引で引く。行への変換 (SQLite の行 / メモリ上の本文) は呼ぶ側
pub struct IndexRetriever<'a, F> {
    pub index: &'a Index,
    pub scopes: Option<&'a [String]>,
    pub rows: F,
}

impl<R: Text, E, F: Fn(&Index, &[Scored]) -> Result<Vec<R>, E>> Retriever for IndexRetriever<'_, F> {
    type Row = R;
    type Error = E;
    fn find_all(&self, terms: &[String], limit: usize, offset: usize) -> Result<(Vec<R>, usize), E> {
        let (hits, total) = self.index.search(terms, self.scopes, Match::All, limit, offset);
        Ok(((self.rows)(self.index, &hits)?, total))
    }
    fn find_loose(&self, terms: &[String], limit: usize, offset: usize) -> Result<(Vec<R>, usize), E> {
        let (hits, total) = self.index.search(terms, self.scopes, Match::Loose, limit, offset);
        Ok(((self.rows)(self.index, &hits)?, total))
    }
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Exact,
    Corrected,
    Loose,
    None,
}

#[derive(Serialize, Debug, Clone)]
pub struct Change {
    pub from: String,
    pub to: String,
}

pub struct Outcome<R> {
    pub mode: Mode,
    pub rows: Vec<R>,
    pub total: usize,
    /// 実際に引いた語
    pub terms: Vec<String>,
    /// 自動補正したときの、補正後のクエリ (表示用)
    pub corrected: Option<String>,
    pub changes: Vec<Change>,
    /// もしかして (表示用のクエリ)
    pub suggestion: Option<String>,
}

impl<R> Outcome<R> {
    /// 抜粋で強調する語。「を」「の」のような 1 文字のひらがなは強調しない (抜粋が助詞だらけになる)
    pub fn marks(&self) -> Vec<String> {
        let m: Vec<String> = self
            .terms
            .iter()
            .filter(|t| !(t.chars().count() == 1 && t.chars().all(crate::normalize::is_hiragana)))
            .cloned()
            .collect();
        if m.is_empty() { self.terms.clone() } else { m }
    }
}

pub fn run<R: Retriever>(r: &R, vocab: &Vocab, q: &str, exact: bool, limit: usize, offset: usize) -> Result<Outcome<R::Row>, R::Error> {
    let plan = crate::plan(q, vocab);
    let mut out = Outcome {
        mode: Mode::None,
        rows: Vec::new(),
        total: 0,
        terms: Vec::new(),
        corrected: None,
        changes: Vec::new(),
        suggestion: None,
    };
    let first = plan.exact();
    if first.is_empty() {
        return Ok(out);
    }

    // 1. そのまま
    let (rows, total) = r.find_all(&first, limit, offset)?;
    if total > 0 || exact {
        out.mode = if total > 0 { Mode::Exact } else { Mode::None };
        (out.rows, out.total, out.terms) = (rows, total, first);
        out.suggestion = suggestion_text(r, &plan, q)?;
        return Ok(out);
    }

    // 2. 自動補正
    if let Some(fixed) = plan.corrected() {
        let (rows, total) = r.find_all(&fixed, limit, offset)?;
        if total > 0 {
            out.changes = changes(&plan, &rows);
            out.corrected = Some(display(&plan, &rows, Fix::Auto));
            out.mode = Mode::Corrected;
            (out.rows, out.total, out.terms) = (rows, total, fixed);
            out.suggestion = suggestion_text(r, &plan, q)?;
            return Ok(out);
        }
    }

    // 3. 語の一部の一致 (最後の安全網)
    let (rows, total) = r.find_loose(&first, limit, offset)?;
    if total > 0 {
        out.mode = Mode::Loose;
        (out.rows, out.total, out.terms) = (rows, total, first);
    }
    out.suggestion = suggestion_text(r, &plan, q)?;
    Ok(out)
}

/// 正規化済みの語を、文書の中での表記に戻す (k8s → K8s, ロールバック はそのまま)
fn pretty<T: Text>(term: &str, rows: &[T]) -> String {
    rows.iter()
        .find_map(|r| surface_bounded(r.title(), term).or_else(|| surface_bounded(r.body(), term)))
        .or_else(|| rows.iter().find_map(|r| surface(r.title(), term).or_else(|| surface(r.body(), term))))
        .unwrap_or_else(|| term.to_string())
}

fn changes<T: Text>(plan: &Plan, rows: &[T]) -> Vec<Change> {
    plan.terms
        .iter()
        .filter(|t| t.fix == Fix::Auto)
        .map(|t| Change { from: t.term.clone(), to: merge_parts(&t.to, rows) })
        .collect()
}

/// 分けた漢字の断片 (検索 / 索手 / 手順) を 1 語に戻して表記を整える
fn merge_parts<T: Text>(parts: &[String], rows: &[T]) -> String {
    if parts.len() == 1 {
        return pretty(&parts[0], rows);
    }
    let mut s: String = parts[0].clone();
    for p in &parts[1..] {
        s.extend(p.chars().skip(1));
    }
    s
}

/// 補正を当てたクエリを、表示用に組み立てる
fn display<T: Text>(plan: &Plan, rows: &[T], upto: Fix) -> String {
    plan.terms
        .iter()
        .map(|t| {
            let apply = t.fix == Fix::Auto || (upto == Fix::Suggest && t.fix == Fix::Suggest);
            if apply { merge_parts(&t.to, rows) } else { t.term.clone() }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn suggestion_text<R: Retriever>(r: &R, plan: &Plan, q: &str) -> Result<Option<String>, R::Error> {
    let Some(terms) = plan.suggestion() else { return Ok(None) };
    // 提案した語で本当に見つかるときだけ出す
    let (rows, total) = r.find_all(&terms, 3, 0)?;
    if total == 0 {
        return Ok(None);
    }
    let s = display(plan, &rows, Fix::Suggest);
    Ok((crate::normalize(&s) != crate::normalize(q)).then_some(s))
}
