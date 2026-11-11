//! ローカル HTTP サーバ。127.0.0.1 にだけ bind し、Host ヘッダも確認する
//! (DNS リバインディング対策)。状態を変える POST には独自ヘッダを必須にする (CSRF 対策)。

use crate::search::{Engine, Searcher};
use crate::{db, indexer, wasm::ExtractorHost};
use axum::{
    Json, Router,
    body::Body,
    extract::{Path as AxPath, Query, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::json;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

const INDEX_HTML: &str = include_str!("../ui/index.html");

pub struct App {
    pub port: u16,
    pub db_path: PathBuf,
    pub roots: Vec<PathBuf>,
    pub sources: indexer::Sources,
    pub engine: Engine,
    /// 見てよい閲覧範囲。None なら全部
    pub scopes: Option<Vec<String>>,
    pub read: Mutex<Connection>,
    pub extractor: Arc<ExtractorHost>,
    pub indexing: AtomicBool,
    pub last: Mutex<Option<indexer::Report>>,
    /// 自前の索引と語の一覧。索引が変わったら作り直す
    pub searcher: RwLock<Arc<Searcher>>,
}

impl App {
    pub fn reload(&self) -> anyhow::Result<()> {
        let index = indexer::load_index(&self.db_path, &self.extractor)?;
        let s = {
            let conn = self.read.lock().unwrap();
            Searcher::load(self.engine, &conn, index)?
        };
        *self.searcher.write().unwrap() = Arc::new(s);
        Ok(())
    }

    fn searcher(&self) -> Arc<Searcher> {
        self.searcher.read().unwrap().clone()
    }

    fn can_see(&self, scope: &str) -> bool {
        self.scopes.as_ref().is_none_or(|sc| sc.iter().any(|s| s == scope))
    }

    /// 見てよい文書か。見てはいけない文書は「ない」ことにする (存在も漏らさない)
    fn check_visible(&self, conn: &Connection, id: i64) -> Result<(), (StatusCode, String)> {
        match db::scope_of(conn, id).map_err(err500)? {
            Some(s) if self.can_see(&s) => Ok(()),
            _ => Err((StatusCode::NOT_FOUND, "not found".into())),
        }
    }
}

type S = State<Arc<App>>;
type ApiResult = Result<Response, (StatusCode, String)>;

fn err500<E: std::fmt::Display>(e: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/search", get(index))
        .route("/doc/{id}", get(index))
        .route("/api/search", get(search))
        .route("/api/suggest", get(suggest))
        .route("/api/doc/{id}", get(doc))
        .route("/api/recent", get(recent))
        .route("/api/status", get(status))
        .route("/api/reindex", post(reindex))
        .route("/api/resolve", get(resolve))
        .route("/api/file", get(file))
        .route("/api/reveal/{id}", post(reveal))
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}

async fn guard(State(app): S, req: Request, next: Next) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let ok_hosts = [
        format!("127.0.0.1:{}", app.port),
        format!("localhost:{}", app.port),
    ];
    if !ok_hosts.iter().any(|h| h == host) {
        return (StatusCode::FORBIDDEN, "bad host").into_response();
    }
    if req.method() == axum::http::Method::POST && !req.headers().contains_key("x-lb") {
        return (StatusCode::FORBIDDEN, "missing x-lb header").into_response();
    }
    next.run(req).await
}

async fn index() -> impl IntoResponse {
    (
        [(header::CACHE_CONTROL, "no-cache")],
        Html(INDEX_HTML),
    )
}

#[derive(Deserialize)]
struct SearchQ {
    q: String,
    #[serde(default)]
    page: usize,
    /// 1 なら補正しない (「元の語のまま検索」)
    #[serde(default)]
    exact: u8,
}

async fn search(State(app): S, Query(p): Query<SearchQ>) -> ApiResult {
    let started = Instant::now();
    let per = 20;
    let searcher = app.searcher();
    let conn = app.read.lock().unwrap();
    let o = searcher.run(&conn, app.scopes.as_deref(), &p.q, p.exact == 1, per, p.page * per).map_err(err500)?;
    Ok(Json(json!({
        "q": p.q,
        "page": p.page,
        "per": per,
        "total": o.total,
        "hits": o.hits,
        "mode": o.mode,
        "terms": o.terms,
        "corrected": o.corrected,
        "changes": o.changes,
        "suggestion": o.suggestion,
        "engine": searcher.engine,
        "millis": started.elapsed().as_secs_f64() * 1000.0,
    }))
    .into_response())
}

async fn suggest(State(app): S, Query(p): Query<SearchQ>) -> ApiResult {
    let searcher = app.searcher();
    let conn = app.read.lock().unwrap();
    let hits = searcher.run(&conn, app.scopes.as_deref(), &p.q, false, 8, 0).map_err(err500)?.hits;
    let items: Vec<_> = hits
        .into_iter()
        .map(|h| json!({"id": h.id, "title": h.title, "rel": h.rel}))
        .collect();
    Ok(Json(items).into_response())
}

async fn doc(State(app): S, AxPath(id): AxPath<i64>) -> ApiResult {
    let conn = app.read.lock().unwrap();
    match db::get(&conn, id).map_err(err500)? {
        Some(d) if app.can_see(&d.scope) => Ok(Json(d).into_response()),
        _ => Err((StatusCode::NOT_FOUND, "not found".into())),
    }
}

async fn recent(State(app): S) -> ApiResult {
    let conn = app.read.lock().unwrap();
    Ok(Json(db::recent(&conn, app.scopes.as_deref(), 12).map_err(err500)?).into_response())
}

async fn status(State(app): S) -> ApiResult {
    let count = {
        let conn = app.read.lock().unwrap();
        db::count(&conn).map_err(err500)?
    };
    let last = app.last.lock().unwrap().clone();
    Ok(Json(json!({
        "docs": count,
        "roots": app.roots,
        "indexing": app.indexing.load(Ordering::SeqCst),
        "last": last,
        "extensions": app.extractor.extensions(),
        "vocab": app.searcher().vocab.len(),
        "engine": app.engine,
    }))
    .into_response())
}

async fn reindex(State(app): S) -> ApiResult {
    let started = start_index(app);
    Ok(Json(json!({ "started": started })).into_response())
}

/// 索引更新を別スレッドで開始する。すでに実行中なら false。
pub fn start_index(app: Arc<App>) -> bool {
    if app
        .indexing
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return false;
    }
    std::thread::spawn(move || {
        let res = indexer::run(&app.db_path, &app.sources, &app.extractor);
        match res {
            Ok(rep) => {
                let c = &rep.counts;
                if c.added + c.updated + c.deleted + c.failed + c.same_content > 0 || rep.merged {
                    eprintln!(
                        "[index] +{} ~{} -{} !{} (={} 変化なし, {} 中身は同じ, キャッシュ {}) セグメント {} 世代 {} {}ms",
                        c.added, c.updated, c.deleted, c.failed, c.unchanged, c.same_content,
                        rep.cache_hits, rep.segments, rep.generation, rep.millis
                    );
                    for e in &rep.errors {
                        eprintln!("  ! {e}");
                    }
                }
                if rep.changed() || app.searcher().vocab.is_empty() {
                    if let Err(e) = app.reload() {
                        eprintln!("[index] 読み込みエラー: {e:#}");
                    }
                }
                *app.last.lock().unwrap() = Some(rep);
            }
            Err(e) => eprintln!("[index] error: {e:#}"),
        }
        app.indexing.store(false, Ordering::SeqCst);
    });
    true
}

#[derive(Deserialize)]
struct ResolveQ {
    from: i64,
    href: String,
}

/// 文書内の相対リンクを解決する。索引済み文書なら {id}、それ以外の root 内ファイルなら {file}
async fn resolve(State(app): S, Query(p): Query<ResolveQ>) -> ApiResult {
    let conn = app.read.lock().unwrap();
    app.check_visible(&conn, p.from)?;
    let target = resolve_path(&app, &conn, p.from, &p.href)?;
    let key = target.to_string_lossy().into_owned();
    if let Some(id) = db::id_of_path(&conn, &key).map_err(err500)? {
        return Ok(Json(json!({ "id": id })).into_response());
    }
    if target.is_file() {
        return Ok(Json(json!({ "file": format!("/api/file?from={}&href={}", p.from, urlencode(&p.href)) })).into_response());
    }
    Err((StatusCode::NOT_FOUND, "not found".into()))
}

async fn file(State(app): S, Query(p): Query<ResolveQ>) -> ApiResult {
    let target = {
        let conn = app.read.lock().unwrap();
        app.check_visible(&conn, p.from)?;
        resolve_path(&app, &conn, p.from, &p.href)?
    };
    let bytes = tokio::fs::read(&target)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "not found".to_string()))?;
    let mime = mime_guess::from_path(&target).first_or_octet_stream();
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, mime.to_string().parse().unwrap());
    // ファイル内のスクリプトがこのオリジンで動かないように
    headers.insert(header::CONTENT_SECURITY_POLICY, "sandbox".parse().unwrap());
    headers.insert("x-content-type-options", "nosniff".parse().unwrap());
    Ok((headers, Body::from(bytes)).into_response())
}

fn resolve_path(app: &App, conn: &Connection, from: i64, href: &str) -> Result<PathBuf, (StatusCode, String)> {
    let base = db::path_of(conn, from)
        .map_err(err500)?
        .ok_or((StatusCode::NOT_FOUND, "no such doc".to_string()))?;
    let base = PathBuf::from(base);
    let root = app
        .roots
        .iter()
        .find(|r| base.starts_with(r))
        .ok_or((StatusCode::FORBIDDEN, "outside roots".to_string()))?;
    let href = href.split(['#', '?']).next().unwrap_or("");
    let href = percent_decode(href);
    if href.is_empty() {
        return Ok(base);
    }
    let joined = if let Some(abs) = href.strip_prefix('/') {
        root.join(abs)
    } else {
        base.parent().unwrap_or(root).join(&href)
    };
    let norm = normalize(&joined);
    // シンボリックリンクで外へ出ていないかも確認
    let real = norm.canonicalize().unwrap_or(norm.clone());
    if !app.roots.iter().any(|r| norm.starts_with(r) && real.starts_with(r)) {
        return Err((StatusCode::FORBIDDEN, "outside roots".into()));
    }
    Ok(norm)
}

async fn reveal(State(app): S, AxPath(id): AxPath<i64>) -> ApiResult {
    let path = {
        let conn = app.read.lock().unwrap();
        app.check_visible(&conn, id)?;
        db::path_of(&conn, id).map_err(err500)?
    }
    .ok_or((StatusCode::NOT_FOUND, "not found".to_string()))?;
    let cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg("-R").arg(&path).spawn()
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("explorer").arg(format!("/select,{path}")).spawn()
    } else {
        let dir = Path::new(&path).parent().map(|p| p.to_path_buf()).unwrap_or_default();
        std::process::Command::new("xdg-open").arg(dir).spawn()
    };
    cmd.map_err(err500)?;
    Ok(Json(json!({"ok": true})).into_response())
}

fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() + 0 && i + 2 <= b.len() - 1 {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
