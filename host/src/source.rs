//! 取り込み元。ローカルのディレクトリと、Confluence (今は模擬データ)。
//!
//! どちらも「key / 版 / 閲覧範囲 / 中身の取り出し方」の並びに揃える。
//! 版は最初の足切りに使い、本当に変わったかは中身の BLAKE3 で決める。
//! 本物の Confluence コネクタは段階 5 で WASI コンポーネントとして作る。
//! 模擬データはそのときの形 (ページ ID・版番号・閲覧制限) を先に試すためのもの。

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use walkdir::{DirEntry, WalkDir};

const MAX_FILE_SIZE: u64 = 8 << 20;
const SKIP_DIRS: &[&str] = &["node_modules", "target", "dist", "build", "vendor", "__pycache__"];
pub const PUBLIC: &str = "public";

pub struct Item {
    /// ファイルは絶対パス、Confluence は confluence:SPACE/ID
    pub key: String,
    pub root: String,
    /// 画面に出す相対的な名前
    pub rel: String,
    /// 抽出 wasm に渡す名前 (拡張子で形式を決める)
    pub extract_name: String,
    pub ext: String,
    pub version: String,
    pub mtime: i64,
    pub scope: String,
    pub fetch: Fetch,
}

pub enum Fetch {
    File(PathBuf),
    Inline(Vec<u8>),
}

impl Fetch {
    pub fn bytes(&self) -> Result<Vec<u8>> {
        match self {
            Fetch::File(p) => std::fs::read(p).with_context(|| p.display().to_string()),
            Fetch::Inline(b) => Ok(b.clone()),
        }
    }
}

pub fn scan_dir(root: &Path, exts: &[String]) -> Vec<Item> {
    let root_key = root.to_string_lossy().into_owned();
    let mut out = Vec::new();
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !skip(e))
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let path = entry.path();
        let Some(ext) = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()) else { continue };
        if !exts.iter().any(|x| *x == ext) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if meta.len() > MAX_FILE_SIZE {
            continue;
        }
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy().into_owned();
        out.push(Item {
            key: path.to_string_lossy().into_owned(),
            root: root_key.clone(),
            extract_name: rel.clone(),
            rel,
            ext,
            version: format!("{mtime}-{}", meta.len()),
            mtime,
            scope: PUBLIC.into(),
            fetch: Fetch::File(path.to_path_buf()),
        });
    }
    out
}

fn skip(e: &DirEntry) -> bool {
    if e.depth() == 0 {
        return false;
    }
    let name = e.file_name().to_string_lossy();
    name.starts_with('.') || (e.file_type().is_dir() && SKIP_DIRS.contains(&name.as_ref()))
}

// ---------------------------------------------------------------- Confluence (模擬)

#[derive(Deserialize)]
struct Fixture {
    spaces: Vec<Space>,
}

#[derive(Deserialize)]
struct Space {
    key: String,
    pages: Vec<Page>,
}

#[derive(Deserialize)]
struct Page {
    id: String,
    version: u64,
    title: String,
    /// UNIX 秒
    updated_at: i64,
    /// 閲覧制限 (グループ名)。なければ公開
    #[serde(default)]
    restricted_to: Option<String>,
    /// storage 形式 (XHTML)
    body: String,
}

pub fn confluence_fixture(path: &Path) -> Result<Vec<Item>> {
    let f: Fixture = serde_json::from_slice(&std::fs::read(path).with_context(|| path.display().to_string())?)
        .with_context(|| format!("Confluence の模擬データが不正です: {}", path.display()))?;
    let mut out = Vec::new();
    for s in f.spaces {
        for p in s.pages {
            let html = format!(
                "<html><head><meta charset=\"utf-8\"><title>{}</title></head><body>{}</body></html>",
                escape(&p.title),
                p.body
            );
            out.push(Item {
                key: format!("confluence:{}/{}", s.key, p.id),
                root: format!("confluence:{}", s.key),
                rel: format!("{} / {}", s.key, p.title),
                extract_name: format!("{}-{}.html", s.key, p.id),
                ext: "html".into(),
                version: format!("v{}", p.version),
                mtime: p.updated_at,
                scope: p.restricted_to.unwrap_or_else(|| PUBLIC.into()),
                fetch: Fetch::Inline(html.into_bytes()),
            });
        }
    }
    Ok(out)
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}
