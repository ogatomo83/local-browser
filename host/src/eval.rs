//! 黄金のクエリ集で検索の質を確かめる (`lb eval golden/queries.json docs/test`)。
//!
//! クエリ集は「探したい語と、出てきてほしい文書」の組。打ち間違い・表記の揺れ・
//! 2 文字の漢字語に加えて、「直してはいけない語」と「何も出てはいけない語」も入れる。
//! 補正の規則を変えたら必ず回す。

use crate::search::{Engine, Searcher};
use crate::{db, indexer, wasm::ExtractorHost};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Deserialize)]
struct Case {
    q: String,
    #[serde(default)]
    kind: String,
    /// 上位 k 件のどれかの rel がこのどれかを含むこと
    #[serde(default)]
    top: Vec<String>,
    #[serde(default = "default_k")]
    k: usize,
    /// 期待する段階 (exact / corrected / loose / none)
    #[serde(default)]
    mode: Option<String>,
    /// 補正後のクエリ (表示用)
    #[serde(default)]
    corrected: Option<String>,
    /// 「もしかして」の内容
    #[serde(default)]
    suggest: Option<String>,
    #[serde(default)]
    note: String,
    /// まだ解けない (先の段階で解く) ことが分かっている問題。失敗しても合否に数えない
    #[serde(default)]
    pending: bool,
    /// この閲覧範囲だけで検索する (権限の確認用)
    #[serde(default)]
    scopes: Option<Vec<String>>,
    /// 上位 k 件にこれを含んではいけない
    #[serde(default)]
    not_top: Vec<String>,
}

fn default_k() -> usize {
    3
}

pub fn run(golden: &Path, db_path: &Path, sources: &indexer::Sources, ex: &ExtractorHost, engine: Engine, scopes: Option<&[String]>) -> Result<bool> {
    let cases: Vec<Case> = serde_json::from_str(
        &std::fs::read_to_string(golden).with_context(|| format!("{} を読めません", golden.display()))?,
    )
    .context("クエリ集の JSON が不正です")?;
    let rep = indexer::run(db_path, sources, ex)?;
    let conn = db::open(db_path)?;
    let searcher = Searcher::load(engine, &conn, indexer::load_index(db_path, ex)?)?;
    println!(
        "engine: {engine:?} / 索引: {} 文書, セグメント {}, 語の一覧: {} 語 ({}ms)\n",
        rep.counts.added + rep.counts.updated + rep.counts.unchanged,
        rep.segments,
        searcher.vocab.len(),
        rep.millis
    );
    let mut micros = Vec::new();

    let mut fails = 0;
    let mut pending = (0, 0);
    let mut by_kind: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
    for c in &cases {
        let t = std::time::Instant::now();
        let o = searcher.run(&conn, c.scopes.as_deref().or(scopes), &c.q, false, 10, 0)?;
        micros.push(t.elapsed().as_micros());
        let mut why = Vec::new();
        let mode = format!("{:?}", o.mode).to_lowercase();
        if let Some(m) = &c.mode {
            if *m != mode {
                why.push(format!("段階 {mode} (期待 {m})"));
            }
        }
        if !c.top.is_empty() {
            let got = o.hits.iter().take(c.k).any(|h| c.top.iter().any(|t| h.rel.contains(t.as_str())));
            if !got {
                why.push(format!("上位 {} 件に {:?} がない", c.k, c.top));
            }
        }
        if let Some(bad) = o.hits.iter().take(c.k).find(|h| c.not_top.iter().any(|t| h.rel.contains(t.as_str()))) {
            why.push(format!("上位 {} 件に出てはいけない {} が出た", c.k, bad.rel));
        }
        if let Some(want) = &c.corrected {
            if o.corrected.as_deref() != Some(want.as_str()) {
                why.push(format!("補正 {:?} (期待 {want:?})", o.corrected));
            }
        }
        if let Some(want) = &c.suggest {
            if o.suggestion.as_deref() != Some(want.as_str()) {
                why.push(format!("もしかして {:?} (期待 {want:?})", o.suggestion));
            }
        }
        let e = by_kind.entry(c.kind.clone()).or_default();
        e.1 += 1;
        let top: Vec<&str> = o.hits.iter().take(3).map(|h| h.rel.as_str()).collect();
        let fix = o.corrected.as_ref().map(|s| format!(" → {s}")).unwrap_or_default();
        let sug = o.suggestion.as_ref().map(|s| format!(" (もしかして {s})")).unwrap_or_default();
        if c.pending {
            pending.1 += 1;
            if why.is_empty() {
                pending.0 += 1;
            }
            let mark = if why.is_empty() { "ok" } else { "--" };
            println!("  {mark}  {:<22} {:<9} {}件{fix}{sug} {:?} (保留: {})", c.q, mode, o.total, top, c.note);
            e.1 -= 1;
            continue;
        }
        if why.is_empty() {
            e.0 += 1;
            println!("  ok  {:<22} {:<9} {}件{fix}{sug}", c.q, mode, o.total);
        } else {
            fails += 1;
            println!("  NG  {:<22} {:<9} {}件{fix}{sug} {:?}", c.q, mode, o.total, top);
            for w in why {
                println!("        - {w}");
            }
            if !c.note.is_empty() {
                println!("        ({})", c.note);
            }
        }
    }
    println!();
    for (k, (ok, n)) in by_kind.iter().filter(|(_, (_, n))| *n > 0) {
        println!("  {k:<12} {ok}/{n}");
    }
    micros.sort_unstable();
    let p50 = micros.get(micros.len() / 2).copied().unwrap_or(0);
    let max = micros.last().copied().unwrap_or(0);
    println!("\n検索時間: 中央値 {:.2}ms / 最大 {:.2}ms", p50 as f64 / 1000.0, max as f64 / 1000.0);
    let n = cases.len() - pending.1;
    println!("\n合計 {}/{n}  (保留 {} 本のうち {} 本は今でも通る)", n - fails, pending.1, pending.0);
    Ok(fails == 0)
}
