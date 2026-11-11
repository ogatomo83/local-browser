//! モード C のブラウザ用 wasm (lb-web)。Web Worker の中で動く。
//!
//! 検索・取り込みの判断・抽出は lb と同じ lb-core / lb-extract。ここは
//! 「JS が読んだファイルのバイト列を受け取り、置くべきもの (セグメント・抽出結果・manifest) を返す」だけ。
//! ファイルを読むのも IndexedDB に置くのも JS の仕事で、wasm は外の世界に触れない (import が 1 つもない)。
//!
//! やりとりは `lb_call(命令の JSON, バイト列)` の 1 本。結果は JSON かバイト列。
//!
//! 起動:    open(manifest) → segment × n → blobs → blob × n → ready
//! 取り込み: scan(ファイルの一覧) → put(読み直すファイル) × n → commit → take(置くもの) × n
//! 検索:    search / recent / status

use lb_core::doc::Extracted;
use lb_core::index::{Index, Scored};
use lb_core::ingest::{self, Ingest, MAX_SEGMENTS, Source};
use lb_core::manifest::Manifest;
use lb_core::search::{IndexRetriever, Text};
use lb_core::segment::Segment;
use lb_core::Vocab;
use serde::Deserialize;
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

pub const PUBLIC: &str = "public";

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Cmd {
    /// data = 保存してある manifest (なければ空)。build = この wasm の版 (中身のハッシュ)
    Open { build: String },
    Segment { id: String },
    Blobs,
    Blob { id: String },
    Ready,
    Scan { items: Vec<ScanItem> },
    /// data = ファイルの中身。name は抽出に渡す名前 (拡張子で形式を決める)
    Put { key: String, version: String, #[serde(default)] scope: Option<String>, mtime: i64, name: String },
    Failed { key: String },
    Commit,
    Take { id: String },
    Search { q: String, #[serde(default)] exact: bool, #[serde(default = "per")] limit: usize, #[serde(default)] offset: usize, #[serde(default)] scopes: Option<Vec<String>> },
    Recent { #[serde(default = "per")] limit: usize },
    Doc { id: String },
    Status,
}

fn per() -> usize {
    20
}

#[derive(Deserialize)]
pub struct ScanItem {
    pub key: String,
    pub version: String,
    #[serde(default)]
    pub scope: Option<String>,
    pub mtime: i64,
    /// 画面に出す名前 (なければ key の 1 つめの / より後ろ)
    #[serde(default)]
    pub rel: Option<String>,
}

pub enum Out {
    Json(Value),
    Bytes(Vec<u8>),
}

struct DocText {
    title: String,
    body: String,
}

#[derive(Default)]
pub struct State {
    index_version: String,
    manifest: Manifest,
    /// 索引に入っていないセグメント (読み込み途中 / 開き直しの間)
    segs: HashMap<String, Segment>,
    index: Option<Index>,
    vocab: Vocab,
    /// 抽出結果の名前 → 抜粋に使う本文
    texts: HashMap<String, DocText>,
    rels: HashMap<String, String>,
    ingest: Option<Ingest>,
    /// index を開いたときのセグメントの名前 (index の中の順)
    opened: Vec<String>,
    /// JS に置いてもらうもの (名前 → バイト列)
    outbox: HashMap<String, Vec<u8>>,
}

struct Row<'a> {
    key: &'a str,
    blob: &'a str,
    text: Option<&'a DocText>,
}

impl Text for Row<'_> {
    fn title(&self) -> &str {
        self.text.map_or("", |t| &t.title)
    }
    fn body(&self) -> &str {
        self.text.map_or("", |t| &t.body)
    }
}

impl State {
    pub fn call(&mut self, cmd: Cmd, data: &[u8]) -> Result<Out, String> {
        Ok(Out::Json(match cmd {
            Cmd::Open { build } => {
                *self = State { index_version: format!("seg1/web/norm{}/{build}", lb_core::NORM_VERSION), ..Default::default() };
                let saved: Option<Manifest> = serde_json::from_slice(data).ok();
                let reset = match saved {
                    Some(m) if m.index_version == self.index_version && m.format == lb_core::manifest::MANIFEST_FORMAT => {
                        self.manifest = m;
                        false
                    }
                    other => {
                        self.manifest = Manifest::new(&self.index_version);
                        other.is_some()
                    }
                };
                let ids: Vec<&str> = self.manifest.segments.iter().map(|s| s.id.as_str()).collect();
                json!({ "segments": ids, "reset": reset, "index_version": self.index_version })
            }
            Cmd::Segment { id } => {
                if lb_core::hash::content_id(data) != id {
                    return Err(format!("セグメントの中身が名前と合いません: {id}"));
                }
                self.segs.insert(id, Segment::from_bytes(data)?);
                json!({})
            }
            Cmd::Blobs => {
                self.reopen()?;
                let ix = self.index.as_ref().unwrap();
                let ids: HashSet<&str> = ix.live_docs().map(|(_, d)| d.hash.as_str()).filter(|h| !self.texts.contains_key(*h)).collect();
                json!({ "ids": ids })
            }
            Cmd::Blob { id } => {
                let d = Extracted::from_blob(data)?;
                self.texts.insert(id, DocText { title: d.title, body: d.body });
                json!({})
            }
            Cmd::Ready => {
                self.vocab = self.index.as_ref().map(|ix| ix.vocab()).unwrap_or_default();
                self.status()
            }
            Cmd::Scan { items } => {
                let mut ing = Ingest::new(self.manifest.clone());
                let mut read = Vec::new();
                for it in items {
                    let src = Source { key: it.key.clone(), version: it.version, scope: it.scope.unwrap_or_else(|| PUBLIC.into()), mtime: it.mtime };
                    if let Some(r) = it.rel {
                        self.rels.insert(it.key.clone(), r);
                    }
                    if ing.check(&src) {
                        read.push(it.key);
                    }
                }
                self.ingest = Some(ing);
                json!({ "read": read })
            }
            Cmd::Put { key, version, scope, mtime, name } => {
                let ing = self.ingest.as_mut().ok_or("scan の前に put が来ました")?;
                let src = Source { key, version, scope: scope.unwrap_or_else(|| PUBLIC.into()), mtime };
                let hash = lb_core::hash::content_id(data);
                if ing.same_content(&src, &hash) {
                    return Ok(Out::Json(json!({ "status": "same" })));
                }
                match lb_extract::extract(&name, data) {
                    Ok(d) => {
                        let doc = Extracted {
                            title: d.title,
                            headings: d.headings.into_iter().map(|h| lb_core::doc::HeadingOut { level: h.level, text: h.text, anchor: h.anchor }).collect(),
                            body: d.body,
                            html: d.html,
                        };
                        let (id, bytes) = doc.to_blob();
                        ing.add(&src, &hash, &id, &doc);
                        self.texts.insert(id.clone(), DocText { title: doc.title, body: doc.body });
                        self.outbox.insert(id.clone(), bytes);
                        json!({ "status": "new", "id": id })
                    }
                    Err(e) => {
                        ing.failed();
                        json!({ "status": "failed", "error": e })
                    }
                }
            }
            Cmd::Failed { key: _ } => {
                self.ingest.as_mut().ok_or("scan の前に failed が来ました")?.failed();
                json!({})
            }
            Cmd::Commit => self.commit()?,
            Cmd::Take { id } => return self.outbox.remove(&id).map(Out::Bytes).ok_or(format!("置くものがありません: {id}")),
            Cmd::Search { q, exact, limit, offset, scopes } => self.search(&q, exact, limit, offset, scopes.as_deref())?,
            Cmd::Recent { limit } => {
                let Some(ix) = &self.index else { return Ok(Out::Json(json!([]))) };
                let mut docs: Vec<_> = ix.live_docs().map(|(_, d)| (self.manifest.sources.get(&d.key).map_or(0, |s| s.mtime), d)).collect();
                docs.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.key.cmp(&b.1.key)));
                Value::Array(docs.into_iter().take(limit).map(|(mtime, d)| self.doc_json(d, mtime)).collect())
            }
            Cmd::Doc { id } => {
                let Some(ix) = &self.index else { return Err("not found".into()) };
                let d = ix.live_docs().map(|(_, d)| d).find(|d| d.key == id).ok_or("not found")?;
                self.doc_json(d, self.manifest.sources.get(&d.key).map_or(0, |s| s.mtime))
            }
            Cmd::Status => self.status(),
        }))
    }

    /// manifest のセグメントで索引を開き直す
    fn reopen(&mut self) -> Result<(), String> {
        if let Some(ix) = self.index.take() {
            for (seg, id) in ix.into_segments().into_iter().zip(std::mem::take(&mut self.opened)) {
                self.segs.entry(id).or_insert(seg);
            }
        }
        let mut segs = Vec::new();
        for r in &self.manifest.segments {
            segs.push(self.segs.remove(&r.id).ok_or_else(|| format!("セグメントがありません: {}", r.id))?);
        }
        self.segs.clear();
        self.opened = self.manifest.segments.iter().map(|s| s.id.clone()).collect();
        self.index = Some(Index::new(segs, &self.manifest));
        Ok(())
    }

    fn commit(&mut self) -> Result<Value, String> {
        let ing = self.ingest.take().ok_or("scan の前に commit が来ました")?;
        let done = ing.finish();
        if let Some((r, bytes)) = &done.new_segment {
            self.segs.insert(r.id.clone(), Segment::from_bytes(bytes)?);
            self.outbox.insert(r.id.clone(), bytes.clone());
        }
        self.manifest = done.manifest;
        self.reopen()?;
        let mut merged = false;
        if self.manifest.needs_merge(MAX_SEGMENTS) {
            let m = ingest::merge(self.index.as_ref().unwrap(), &mut self.manifest);
            if let Some((r, bytes)) = m.segment {
                self.segs.insert(r.id.clone(), Segment::from_bytes(&bytes)?);
                self.outbox.insert(r.id, bytes);
            }
            self.index = None;
            self.opened.clear();
            self.reopen()?;
            merged = true;
        }
        let ix = self.index.as_ref().unwrap();
        let keep = ingest::reachable(ix, &self.manifest);
        self.texts.retain(|id, _| keep.contains(id));
        self.outbox.retain(|id, _| keep.contains(id));
        let changed = done.counts.added + done.counts.updated + done.counts.deleted > 0 || merged;
        if changed || self.vocab.is_empty() {
            self.vocab = ix.vocab();
        }
        let put: Vec<&String> = self.outbox.keys().collect();
        Ok(json!({
            "manifest": serde_json::to_value(&self.manifest).map_err(|e| e.to_string())?,
            "save": done.dirty || merged,
            "put": put,
            "keep": keep,
            "counts": done.counts,
            "merged": merged,
            "changed": changed,
            "segments": self.manifest.segments.len(),
            "generation": self.manifest.generation,
            "docs": ix.len(),
        }))
    }

    fn search(&self, q: &str, exact: bool, limit: usize, offset: usize, scopes: Option<&[String]>) -> Result<Value, String> {
        let Some(index) = &self.index else { return Ok(json!({ "mode": "none", "hits": [], "total": 0 })) };
        let texts = &self.texts;
        let rows = |ix: &Index, hits: &[Scored]| -> Result<Vec<Row<'_>>, String> {
            let _ = ix;
            Ok(hits
                .iter()
                .map(|h| {
                    let d = index.doc(h.doc);
                    Row { key: &d.key, blob: &d.hash, text: texts.get(&d.hash) }
                })
                .collect())
        };
        let o = lb_core::search::run(&IndexRetriever { index, scopes, rows }, &self.vocab, q, exact, limit, offset)?;
        let marks = o.marks();
        let hits: Vec<Value> = o
            .rows
            .iter()
            .map(|r| {
                let mtime = self.manifest.sources.get(r.key).map_or(0, |s| s.mtime);
                let (root, rel) = self.names(r.key);
                json!({
                    "id": r.key,
                    "blob": r.blob,
                    "title": r.title(),
                    "title_html": lb_core::highlight::snippet_html(r.title(), &marks, 200),
                    "snippet": lb_core::highlight::snippet_html(r.body(), &marks, 120),
                    "root": root,
                    "rel": rel,
                    "ext": ext_of(r.key),
                    "mtime": mtime,
                })
            })
            .collect();
        Ok(json!({
            "mode": o.mode,
            "hits": hits,
            "total": o.total,
            "terms": o.terms,
            "corrected": o.corrected,
            "changes": o.changes,
            "suggestion": o.suggestion,
        }))
    }

    /// key (フォルダ名/中のパス) → (フォルダ名, 中のパス)
    fn names<'a>(&'a self, key: &'a str) -> (&'a str, &'a str) {
        let (root, rest) = key.split_once('/').unwrap_or(("", key));
        (root, self.rels.get(key).map_or(rest, |s| s.as_str()))
    }

    fn doc_json(&self, d: &lb_core::segment::DocEntry, mtime: i64) -> Value {
        let (root, rel) = self.names(&d.key);
        json!({ "id": d.key, "blob": d.hash, "title": d.title, "root": root, "rel": rel, "ext": ext_of(&d.key), "mtime": mtime, "scope": d.scope })
    }

    fn status(&self) -> Value {
        json!({
            "docs": self.index.as_ref().map_or(0, |ix| ix.len()),
            "segments": self.manifest.segments.len(),
            "generation": self.manifest.generation,
            "vocab": self.vocab.len(),
            "index_version": self.index_version,
            "extensions": lb_extract::EXTENSIONS,
        })
    }
}

fn ext_of(key: &str) -> String {
    let name = key.rsplit('/').next().unwrap_or(key);
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

// ---------------------------------------------------------------- JS とのやりとり (wasm-bindgen なし)

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
    static OUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// JS が引数を書き込む場所を確保する
#[unsafe(no_mangle)]
pub extern "C" fn lb_alloc(len: usize) -> *mut u8 {
    let mut v = Vec::<u8>::with_capacity(len);
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

/// # Safety
/// lb_alloc(len) で得た ptr を 1 度だけ渡すこと
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lb_free(ptr: *mut u8, len: usize) {
    drop(unsafe { Vec::from_raw_parts(ptr, 0, len) });
}

/// 戻り値: 0 = JSON, 1 = バイト列, 2 = エラー (メッセージ)。中身は lb_out_ptr / lb_out_len
///
/// # Safety
/// 引数は lb_alloc で確保して書き込んだ領域
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lb_call(cmd_ptr: *const u8, cmd_len: usize, data_ptr: *const u8, data_len: usize) -> u32 {
    let cmd = unsafe { std::slice::from_raw_parts(cmd_ptr, cmd_len) };
    let data = if data_len == 0 { &[][..] } else { unsafe { std::slice::from_raw_parts(data_ptr, data_len) } };
    let (code, out) = match serde_json::from_slice::<Cmd>(cmd) {
        Err(e) => (2, format!("命令が読めません: {e}").into_bytes()),
        Ok(cmd) => match STATE.with(|s| s.borrow_mut().call(cmd, data)) {
            Ok(Out::Json(v)) => (0, serde_json::to_vec(&v).unwrap_or_default()),
            Ok(Out::Bytes(b)) => (1, b),
            Err(e) => (2, e.into_bytes()),
        },
    };
    OUT.with(|o| *o.borrow_mut() = out);
    code
}

#[unsafe(no_mangle)]
pub extern "C" fn lb_out_ptr() -> *const u8 {
    OUT.with(|o| o.borrow().as_ptr())
}

#[unsafe(no_mangle)]
pub extern "C" fn lb_out_len() -> usize {
    OUT.with(|o| o.borrow().len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(s: &mut State, cmd: Value, data: &[u8]) -> Value {
        match s.call(serde_json::from_value(cmd).unwrap(), data).unwrap() {
            Out::Json(v) => v,
            Out::Bytes(_) => panic!("bytes"),
        }
    }

    fn take(s: &mut State, id: &str) -> Vec<u8> {
        match s.call(Cmd::Take { id: id.into() }, &[]).unwrap() {
            Out::Bytes(b) => b,
            _ => panic!(),
        }
    }

    /// 取り込み → 保存 → 開き直し → リロードで差分だけ、を IndexedDB の代わりに HashMap で
    #[test]
    fn ingest_save_reopen_reload() {
        let mut db: HashMap<String, Vec<u8>> = HashMap::new();
        let mut s = State::default();
        call(&mut s, json!({"op": "open", "build": "t"}), b"");
        let files = [("docs/deploy.md", "# デプロイ手順\n\n本番へのデプロイは平日に。"), ("docs/incident.md", "# 障害対応\n\n当番に連絡する。")];
        let items: Vec<Value> = files.iter().map(|(k, _)| json!({"key": k, "version": "1", "mtime": 1})).collect();
        let r = call(&mut s, json!({"op": "scan", "items": items}), b"");
        assert_eq!(r["read"].as_array().unwrap().len(), 2);
        for (k, body) in files {
            let r = call(&mut s, json!({"op": "put", "key": k, "version": "1", "mtime": 1, "name": k}), body.as_bytes());
            assert_eq!(r["status"], "new");
        }
        let c = call(&mut s, json!({"op": "commit"}), b"");
        assert_eq!(c["counts"]["added"], 2);
        for id in c["put"].as_array().unwrap() {
            let id = id.as_str().unwrap();
            db.insert(id.into(), take(&mut s, id));
        }
        let manifest = serde_json::to_vec(&c["manifest"]).unwrap();
        let r = call(&mut s, json!({"op": "search", "q": "デプロイ"}), b"");
        assert_eq!(r["hits"][0]["id"], "docs/deploy.md");
        assert_eq!(r["hits"][0]["rel"], "deploy.md");

        // ページを開き直す: 保存したものだけで同じ検索ができる
        let mut s = State::default();
        let o = call(&mut s, json!({"op": "open", "build": "t"}), &manifest);
        assert_eq!(o["reset"], false);
        for id in o["segments"].as_array().unwrap() {
            let id = id.as_str().unwrap();
            call(&mut s, json!({"op": "segment", "id": id}), &db[id]);
        }
        let b = call(&mut s, json!({"op": "blobs"}), b"");
        for id in b["ids"].as_array().unwrap() {
            let id = id.as_str().unwrap();
            call(&mut s, json!({"op": "blob", "id": id}), &db[id]);
        }
        let st = call(&mut s, json!({"op": "ready"}), b"");
        assert_eq!(st["docs"], 2);
        let r = call(&mut s, json!({"op": "search", "q": "障害"}), b"");
        assert_eq!(r["hits"][0]["id"], "docs/incident.md");
        assert!(r["hits"][0]["snippet"].as_str().unwrap().contains("当番"));

        // リロード: deploy は変わらず、incident は消えた → 何も読まずに削除の印だけ
        let r = call(&mut s, json!({"op": "scan", "items": [{"key": "docs/deploy.md", "version": "1", "mtime": 1}]}), b"");
        assert!(r["read"].as_array().unwrap().is_empty());
        let c = call(&mut s, json!({"op": "commit"}), b"");
        assert_eq!((c["counts"]["deleted"].as_u64(), c["counts"]["unchanged"].as_u64()), (Some(1), Some(1)));
        let r = call(&mut s, json!({"op": "search", "q": "障害"}), b"");
        assert_eq!(r["total"], 0);

        // 版 (wasm) が変わったら作り直し
        let mut s = State::default();
        let o = call(&mut s, json!({"op": "open", "build": "t2"}), &manifest);
        assert_eq!(o["reset"], true);
    }
}
