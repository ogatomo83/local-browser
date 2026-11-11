//! 検索。段階的な検索 (そのまま → 自動補正 → 語の一部の一致) は lb-core の
//! [`lb_core::search::run`] で、ブラウザ (モード C) と共通。
//!
//! 文書を引く部分は 2 通りあり、--engine で切り替える。
//! - Fts5: SQLite FTS5 (trigram)。段階 2a までのもの。段階 3 で消す
//! - Segments: lb-core の自前の索引 (不変セグメント)。ブラウザでも同じものが動く

use crate::db;
use lb_core::highlight::snippet_html;
use lb_core::index::{Index, Scored};
use lb_core::search::{self as core_search, Change, IndexRetriever, Mode, Retriever, Text};
use lb_core::Vocab;
use rusqlite::Connection;
use serde::Serialize;

#[derive(Serialize)]
pub struct Hit {
    pub id: i64,
    /// HTML (<mark> 以外はエスケープ済み)
    pub title_html: String,
    pub title: String,
    pub root: String,
    pub rel: String,
    pub ext: String,
    pub mtime: i64,
    /// HTML (<mark> 以外はエスケープ済み)
    pub snippet: String,
}

#[derive(Serialize)]
pub struct Outcome {
    pub mode: Mode,
    pub hits: Vec<Hit>,
    pub total: usize,
    /// 実際に引いた語 (文書内の強調にも使う)
    pub terms: Vec<String>,
    pub corrected: Option<String>,
    pub changes: Vec<Change>,
    pub suggestion: Option<String>,
}

/// 文書を引く仕組み (--engine)
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Fts5,
    Segments,
}

impl std::str::FromStr for Engine {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "fts5" => Ok(Engine::Fts5),
            "segments" => Ok(Engine::Segments),
            _ => anyhow::bail!("--engine は fts5 か segments: {s}"),
        }
    }
}

impl Text for db::Row {
    fn title(&self) -> &str {
        &self.title
    }
    fn body(&self) -> &str {
        &self.body
    }
}

/// 検索に必要なものの一式 (索引が変わるたびに作り直す)
pub struct Searcher {
    pub engine: Engine,
    pub index: Index,
    pub vocab: Vocab,
}

impl Searcher {
    pub fn load(engine: Engine, conn: &Connection, index: Index) -> anyhow::Result<Self> {
        let vocab = match engine {
            Engine::Fts5 => db::load_vocab(conn)?,
            Engine::Segments => index.vocab(),
        };
        Ok(Self { engine, index, vocab })
    }

    pub fn run(&self, conn: &Connection, scopes: Option<&[String]>, q: &str, exact: bool, limit: usize, offset: usize) -> anyhow::Result<Outcome> {
        let o = match self.engine {
            Engine::Fts5 => core_search::run(&Fts5 { conn, scopes }, &self.vocab, q, exact, limit, offset)?,
            Engine::Segments => {
                let rows = |ix: &Index, hits: &[Scored]| {
                    let keys: Vec<String> = hits.iter().map(|h| ix.doc(h.doc).key.clone()).collect();
                    db::rows_by_paths(conn, &keys)
                };
                core_search::run(&IndexRetriever { index: &self.index, scopes, rows }, &self.vocab, q, exact, limit, offset)?
            }
        };
        let marks = o.marks();
        let hits = o
            .rows
            .into_iter()
            .map(|r| Hit {
                id: r.id,
                title_html: snippet_html(&r.title, &marks, 200),
                snippet: snippet_html(&r.body, &marks, 120),
                title: r.title,
                root: r.root,
                rel: r.rel,
                ext: r.ext,
                mtime: r.mtime,
            })
            .collect();
        Ok(Outcome { mode: o.mode, hits, total: o.total, terms: o.terms, corrected: o.corrected, changes: o.changes, suggestion: o.suggestion })
    }
}

pub struct Fts5<'a> {
    pub conn: &'a Connection,
    pub scopes: Option<&'a [String]>,
}

impl Retriever for Fts5<'_> {
    type Row = db::Row;
    type Error = anyhow::Error;

    fn find_all(&self, terms: &[String], limit: usize, offset: usize) -> anyhow::Result<(Vec<db::Row>, usize)> {
        db::find_all(self.conn, terms, self.scopes, limit, offset)
    }

    /// 3 文字の断片 (trigram) を半分以上含む文書
    fn find_loose(&self, terms: &[String], limit: usize, offset: usize) -> anyhow::Result<(Vec<db::Row>, usize)> {
        let grams = trigrams(terms);
        if grams.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let need = grams.len().div_ceil(2);
        let rows: Vec<db::Row> = db::find_any_trigrams(self.conn, &grams, self.scopes, 200)?
            .into_iter()
            .filter(|r| {
                let text = lb_core::normalize(&format!("{}\n{}\n{}", r.title, r.headings, r.body));
                grams.iter().filter(|g| text.contains(g.as_str())).count() >= need
            })
            .collect();
        let total = rows.len();
        Ok((rows.into_iter().skip(offset).take(limit).collect(), total))
    }
}

fn trigrams(terms: &[String]) -> Vec<String> {
    let mut v = Vec::new();
    for t in terms {
        let cs: Vec<char> = t.chars().collect();
        for w in cs.windows(3) {
            let g: String = w.iter().collect();
            if !v.contains(&g) {
                v.push(g);
            }
        }
    }
    v
}
