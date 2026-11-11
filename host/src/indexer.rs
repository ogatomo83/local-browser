//! 取り込み。変わったものだけ読み取って、新しいセグメントを 1 つ足す。
//!
//! 何を読み直すか・何を消すかの判断は lb-core の [`Ingest`] (ブラウザのモード C と共通)。
//! ここでは置き場 (`<db>.store/`) と SQLite への読み書きと、抽出 wasm の呼び出しをする。
//! 抽出結果は「中身のハッシュ + 抽出 wasm のハッシュ」でキャッシュしてあるので、
//! 元に戻したファイルや別の場所の同じファイルは wasm を呼ばない。
//!
//! SQLite (docs + FTS5) にも同じ内容を書く。画面の文書表示と、FTS5 との比べ合わせのため。

use crate::source::{self, Item};
use crate::store::{Extracted, HeadingOut, Store};
use crate::{db, wasm::ExtractorHost};
use anyhow::Result;
use lb_core::ingest::{self, Ingest, MAX_SEGMENTS, Source};
use lb_core::manifest::Manifest;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Clone, Default)]
pub struct Sources {
    pub dirs: Vec<PathBuf>,
    pub confluence: Vec<PathBuf>,
}

#[derive(Serialize, Clone, Default)]
pub struct Report {
    #[serde(flatten)]
    pub counts: ingest::Counts,
    /// 抽出結果のキャッシュに当たった (wasm を呼ばなかった)
    pub cache_hits: usize,
    pub errors: Vec<String>,
    pub segments: usize,
    pub merged: bool,
    pub generation: u64,
    pub millis: u128,
    pub finished_at: i64,
}

impl Report {
    pub fn changed(&self) -> bool {
        let c = &self.counts;
        c.added + c.updated + c.deleted > 0 || self.merged
    }
}

pub fn index_version(ex: &ExtractorHost) -> String {
    format!("seg1/schema{}/norm{}/{}", db::schema_version(), lb_core::NORM_VERSION, ex.id)
}

pub fn run(db_path: &Path, sources: &Sources, ex: &ExtractorHost) -> Result<Report> {
    let started = Instant::now();
    let mut conn = db::open(db_path)?;
    let store = Store::for_db(db_path)?;
    let mut manifest = store.load_manifest(&index_version(ex))?;
    // SQLite だけ作り直された (版が変わった) なら、manifest も最初から
    if db::count(&conn)? == 0 && !manifest.sources.is_empty() {
        manifest = Manifest::new(&manifest.index_version);
    }
    let mut rep = Report::default();

    let mut items: Vec<Item> = Vec::new();
    for d in &sources.dirs {
        items.extend(source::scan_dir(d, ex.extensions()));
    }
    for c in &sources.confluence {
        match source::confluence_fixture(c) {
            Ok(v) => items.extend(v),
            Err(e) => rep.errors.push(format!("{e:#}")),
        }
    }

    let mut ing = Ingest::new(manifest);
    let tx = conn.transaction()?;
    for item in &items {
        let src = Source { key: item.key.clone(), version: item.version.clone(), scope: item.scope.clone(), mtime: item.mtime };
        if !ing.check(&src) {
            continue;
        }
        let bytes = match item.fetch.bytes() {
            Ok(b) => b,
            Err(e) => {
                ing.failed();
                rep.errors.push(format!("{}: {e:#}", item.rel));
                continue;
            }
        };
        let hash = lb_core::hash::content_id(&bytes);
        if ing.same_content(&src, &hash) {
            continue;
        }
        let (blob, doc) = match extract(&store, ex, item, &bytes, &hash, &mut rep) {
            Ok(d) => d,
            Err(e) => {
                ing.failed();
                if rep.errors.len() < 50 {
                    rep.errors.push(format!("{}: {e:#}", item.rel));
                }
                continue;
            }
        };
        ing.add(&src, &hash, &blob, &doc);
        let headings_text = doc.headings_text();
        db::upsert(
            &tx,
            &db::NewDoc {
                path: &item.key,
                root: &item.root,
                rel: &item.rel,
                ext: &item.ext,
                scope: &item.scope,
                mtime: item.mtime,
                size: bytes.len() as i64,
                title: &doc.title,
                headings_json: &serde_json::to_string(&doc.headings)?,
                headings_text: &headings_text,
                body: &doc.body,
                html: &doc.html,
            },
        )?;
    }

    let mut done = ing.finish();
    // SQLite 側の取り残し (manifest にないもの) も消す
    for (id, path) in db::all_paths(&tx)? {
        if !done.manifest.sources.contains_key(&path) {
            db::delete(&tx, id)?;
        }
    }
    tx.commit()?;

    // セグメントを置いてから manifest を差し替える
    if let Some((_, bytes)) = &done.new_segment {
        store.put(bytes)?;
    }
    let mut keep = None;
    if done.manifest.needs_merge(MAX_SEGMENTS) {
        let ix = lb_core::index::Index::new(store.load_segments(&done.manifest)?, &done.manifest);
        let merged = ingest::merge(&ix, &mut done.manifest);
        if let Some((_, bytes)) = &merged.segment {
            store.put(bytes)?;
        }
        eprintln!("[store] まとめ直し: {} 文書 → 1 セグメント", ix.len());
        keep = Some(merged.keep);
        rep.merged = true;
    }
    if done.dirty || rep.merged {
        store.save_manifest(&done.manifest)?;
    }
    // manifest を差し替えてから消す (途中で落ちても、manifest が消えた objects を指さないように)
    if let Some(keep) = keep {
        let removed = store.gc(&keep)?;
        eprintln!("[store] 使われなくなった objects を {removed} 個削除");
    }

    rep.counts = done.counts;
    rep.segments = done.manifest.segments.len();
    rep.generation = done.manifest.generation;
    rep.millis = started.elapsed().as_millis();
    rep.finished_at = db::now();
    Ok(rep)
}

fn extract(store: &Store, ex: &ExtractorHost, item: &Item, bytes: &[u8], hash: &str, rep: &mut Report) -> Result<(String, Extracted)> {
    // 同じ中身でも拡張子が違えば読み方が変わるので、拡張子もキーに入れる
    let key = lb_core::hash::key_id(&[hash.as_bytes(), ex.id.as_bytes(), item.ext.as_bytes()]);
    if let Some(id) = store.cached_extract(&key) {
        if let Ok(d) = store.get_extracted(&id) {
            rep.cache_hits += 1;
            return Ok((id, d));
        }
    }
    let d = ex.extract(&item.extract_name, bytes)?;
    let out = Extracted {
        title: d.title,
        headings: d.headings.into_iter().map(|h| HeadingOut { level: h.level, text: h.text, anchor: h.anchor }).collect(),
        body: d.body,
        html: d.html,
    };
    let id = store.put_extracted(&out)?;
    store.remember_extract(&key, &id)?;
    Ok((id, out))
}

/// いまの manifest からセグメントを読んで、自前の索引を開く
pub fn load_index(db_path: &Path, ex: &ExtractorHost) -> Result<lb_core::index::Index> {
    let store = Store::for_db(db_path)?;
    let m = store.load_manifest(&index_version(ex))?;
    let segs = store.load_segments(&m)?;
    Ok(lb_core::index::Index::new(segs, &m))
}
