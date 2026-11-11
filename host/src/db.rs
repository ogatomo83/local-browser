//! SQLite (FTS5 trigram) によるメタ情報と全文索引。
//!
//! 全文索引には lb-core で正規化した文字列を入れる (全角/半角・大文字小文字・長音の揺れを吸収)。
//! 元の文字列は docs に残し、抜粋や表示はそちらから作る。
//! terms は語の一覧 (補正の辞書) の元。語 × 文書 を持ち、df は数えて出す。

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::path::Path;

/// 索引の形の版。正規化の規則の版も含める (どちらかが変われば作り直す)
const SCHEMA: i64 = 3;

pub fn schema_version() -> i64 {
    SCHEMA * 1000 + lb_core::NORM_VERSION as i64
}

pub fn open(path: &Path) -> Result<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")?;
    let ver: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if ver != schema_version() {
        if ver != 0 {
            eprintln!("[db] 索引の版が変わったので作り直します ({ver} → {})", schema_version());
        }
        conn.execute_batch(
            "DROP TABLE IF EXISTS docs_fts; DROP TABLE IF EXISTS terms; DROP TABLE IF EXISTS docs;",
        )?;
    }
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS docs (
            id         INTEGER PRIMARY KEY,
            path       TEXT NOT NULL UNIQUE,
            root       TEXT NOT NULL,
            rel        TEXT NOT NULL,
            ext        TEXT NOT NULL,
            scope      TEXT NOT NULL,
            mtime      INTEGER NOT NULL,
            size       INTEGER NOT NULL,
            title      TEXT NOT NULL,
            headings   TEXT NOT NULL,
            body       TEXT NOT NULL,
            html       TEXT NOT NULL,
            indexed_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS docs_root ON docs(root);
        CREATE INDEX IF NOT EXISTS docs_mtime ON docs(mtime);
        -- trigram: 日本語でも分かち書きなしで部分一致できる (3 文字以上の語)
        CREATE VIRTUAL TABLE IF NOT EXISTS docs_fts USING fts5(
            title, headings, body,
            tokenize = 'trigram'
        );
        CREATE TABLE IF NOT EXISTS terms (
            term   TEXT NOT NULL,
            doc_id INTEGER NOT NULL,
            PRIMARY KEY (term, doc_id)
        ) WITHOUT ROWID;
        CREATE INDEX IF NOT EXISTS terms_doc ON terms(doc_id);
        "#,
    )?;
    conn.pragma_update(None, "user_version", schema_version())?;
    Ok(conn)
}

pub struct NewDoc<'a> {
    pub path: &'a str,
    pub root: &'a str,
    pub rel: &'a str,
    pub ext: &'a str,
    pub scope: &'a str,
    pub mtime: i64,
    pub size: i64,
    pub title: &'a str,
    pub headings_json: &'a str,
    pub headings_text: &'a str,
    pub body: &'a str,
    pub html: &'a str,
}

pub fn upsert(conn: &Connection, d: &NewDoc) -> Result<()> {
    let now = now();
    let existing: Option<i64> = conn
        .query_row("SELECT id FROM docs WHERE path = ?1", [d.path], |r| r.get(0))
        .optional()?;
    let id = match existing {
        Some(id) => {
            conn.execute(
                "UPDATE docs SET root=?2, rel=?3, ext=?4, mtime=?5, size=?6, title=?7,
                 headings=?8, body=?9, html=?10, indexed_at=?11, scope=?12 WHERE id=?1",
                params![id, d.root, d.rel, d.ext, d.mtime, d.size, d.title,
                        d.headings_json, d.body, d.html, now, d.scope],
            )?;
            conn.execute("DELETE FROM docs_fts WHERE rowid = ?1", [id])?;
            conn.execute("DELETE FROM terms WHERE doc_id = ?1", [id])?;
            id
        }
        None => {
            conn.execute(
                "INSERT INTO docs (path, root, rel, ext, mtime, size, title, headings, body, html, indexed_at, scope)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![d.path, d.root, d.rel, d.ext, d.mtime, d.size, d.title,
                        d.headings_json, d.body, d.html, now, d.scope],
            )?;
            conn.last_insert_rowid()
        }
    };
    let (nt, nh, nb) = (
        lb_core::normalize(d.title),
        lb_core::normalize(d.headings_text),
        lb_core::normalize(d.body),
    );
    conn.execute(
        "INSERT INTO docs_fts (rowid, title, headings, body) VALUES (?1, ?2, ?3, ?4)",
        params![id, nt, nh, nb],
    )?;
    let mut st = conn.prepare_cached("INSERT OR IGNORE INTO terms (term, doc_id) VALUES (?1, ?2)")?;
    for t in lb_core::tokenize::index_terms(&format!("{nt}\n{nh}\n{nb}")) {
        st.execute(params![t, id])?;
    }
    Ok(())
}

pub fn delete(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM docs WHERE id = ?1", [id])?;
    conn.execute("DELETE FROM docs_fts WHERE rowid = ?1", [id])?;
    conn.execute("DELETE FROM terms WHERE doc_id = ?1", [id])?;
    Ok(())
}

/// 語の一覧を読み込む。SQLite の TEXT の並び順はバイト順なので、そのまま fst に入れられる
pub fn load_vocab(conn: &Connection) -> Result<lb_core::Vocab> {
    let mut st = conn.prepare("SELECT term, count(*) FROM terms GROUP BY term ORDER BY term")?;
    let rows = st
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    lb_core::Vocab::from_sorted(rows).map_err(anyhow::Error::msg)
}

pub fn all_paths(conn: &Connection) -> Result<Vec<(i64, String)>> {
    let mut st = conn.prepare("SELECT id, path FROM docs")?;
    let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

// ------------------------------------------------------------------ 検索
//
// ここは「正規化済みの語」で文書を引くだけ。どの語で引くか (補正・段階) は search.rs が決める。

pub struct Row {
    pub id: i64,
    pub title: String,
    pub root: String,
    pub rel: String,
    pub ext: String,
    pub mtime: i64,
    pub headings: String,
    pub body: String,
}

const ROW_COLS: &str = "d.id, d.title, d.root, d.rel, d.ext, d.mtime, d.headings, d.body";

fn row(r: &rusqlite::Row) -> rusqlite::Result<Row> {
    Ok(Row {
        id: r.get(0)?,
        title: r.get(1)?,
        root: r.get(2)?,
        rel: r.get(3)?,
        ext: r.get(4)?,
        mtime: r.get(5)?,
        headings: r.get(6)?,
        body: r.get(7)?,
    })
}

fn quote(t: &str) -> String {
    format!("\"{}\"", t.replace('"', "\"\""))
}

fn like_pat(t: &str) -> String {
    format!("%{}%", t.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"))
}

/// すべての語を含む文書 (AND)。trigram は 3 文字未満の語を引けないので、その語は LIKE で絞る
pub fn find_all(conn: &Connection, terms: &[String], scopes: Option<&[String]>, limit: usize, offset: usize) -> Result<(Vec<Row>, usize)> {
    if terms.is_empty() {
        return Ok((Vec::new(), 0));
    }
    let (long, short): (Vec<&String>, Vec<&String>) = terms.iter().partition(|t| t.chars().count() >= 3);
    let mut args: Vec<String> = Vec::new();
    let mut conds: Vec<String> = Vec::new();
    if let Some(sc) = scopes {
        let list = sc.iter().map(|s| format!("'{}'", s.replace('\'', "''"))).collect::<Vec<_>>().join(",");
        conds.push(format!("d.scope IN ({})", if list.is_empty() { "''".to_string() } else { list }));
    }
    if !long.is_empty() {
        args.push(long.iter().map(|t| quote(t)).collect::<Vec<_>>().join(" AND "));
        conds.push(format!("docs_fts MATCH ?{}", args.len()));
    }
    for t in &short {
        args.push(like_pat(t));
        let n = args.len();
        conds.push(format!(
            "(f.title LIKE ?{n} ESCAPE '\\' OR f.headings LIKE ?{n} ESCAPE '\\' OR f.body LIKE ?{n} ESCAPE '\\')"
        ));
    }
    let cond = conds.join(" AND ");
    let order = if long.is_empty() {
        // 短い語だけのときはタイトルに含むものを先に、あとは新しい順
        args.push(like_pat(short[0]));
        format!("(f.title LIKE ?{} ESCAPE '\\') DESC, d.mtime DESC", args.len())
    } else {
        "bm25(docs_fts, 10.0, 4.0, 1.0)".to_string()
    };
    let from = "docs_fts f JOIN docs d ON d.id = f.rowid";
    let n_cond = if long.is_empty() { args.len() - 1 } else { args.len() };
    let total: i64 = conn.query_row(
        &format!("SELECT count(*) FROM {from} WHERE {cond}"),
        rusqlite::params_from_iter(args.iter().take(n_cond)),
        |r| r.get(0),
    )?;
    let sql = format!("SELECT {ROW_COLS} FROM {from} WHERE {cond} ORDER BY {order} LIMIT {limit} OFFSET {offset}");
    let mut st = conn.prepare(&sql)?;
    let rows = st
        .query_map(rusqlite::params_from_iter(args.iter()), row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((rows, total as usize))
}

/// 3 文字の断片 (trigram) を 1 つでも含む文書を、bm25 の順に最大 limit 件
pub fn find_any_trigrams(conn: &Connection, grams: &[String], scopes: Option<&[String]>, limit: usize) -> Result<Vec<Row>> {
    if grams.is_empty() {
        return Ok(Vec::new());
    }
    let expr = grams.iter().map(|g| quote(g)).collect::<Vec<_>>().join(" OR ");
    let scope = match scopes {
        Some(sc) => format!(
            "AND d.scope IN ({})",
            sc.iter().map(|s| format!("'{}'", s.replace('\'', "''"))).chain(std::iter::once("''".into())).collect::<Vec<_>>().join(",")
        ),
        None => String::new(),
    };
    let sql = format!(
        "SELECT {ROW_COLS} FROM docs_fts f JOIN docs d ON d.id = f.rowid
         WHERE docs_fts MATCH ?1 {scope} ORDER BY bm25(docs_fts, 10.0, 4.0, 1.0) LIMIT ?2"
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st
        .query_map(params![expr, limit as i64], row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}


/// key (path) の並びどおりに行を返す (自前の索引の検索結果を、画面用の行にする)
pub fn rows_by_paths(conn: &Connection, paths: &[String]) -> Result<Vec<Row>> {
    let mut st = conn.prepare_cached(&format!("SELECT {ROW_COLS} FROM docs d WHERE d.path = ?1"))?;
    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        if let Some(r) = st.query_row([p], row).optional()? {
            out.push(r);
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------ 取得

#[derive(Serialize)]
pub struct DocView {
    pub id: i64,
    pub path: String,
    pub root: String,
    pub rel: String,
    pub ext: String,
    pub mtime: i64,
    pub size: i64,
    pub title: String,
    pub headings: serde_json::Value,
    pub html: String,
    pub scope: String,
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<DocView>> {
    Ok(conn
        .query_row(
            "SELECT id, path, root, rel, ext, mtime, size, title, headings, html, scope FROM docs WHERE id = ?1",
            [id],
            |r| {
                let h: String = r.get(8)?;
                Ok(DocView {
                    id: r.get(0)?,
                    path: r.get(1)?,
                    root: r.get(2)?,
                    rel: r.get(3)?,
                    ext: r.get(4)?,
                    mtime: r.get(5)?,
                    size: r.get(6)?,
                    title: r.get(7)?,
                    headings: serde_json::from_str(&h).unwrap_or_default(),
                    html: r.get(9)?,
                    scope: r.get(10)?,
                })
            },
        )
        .optional()?)
}

pub fn path_of(conn: &Connection, id: i64) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT path FROM docs WHERE id = ?1", [id], |r| r.get(0))
        .optional()?)
}

pub fn scope_of(conn: &Connection, id: i64) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT scope FROM docs WHERE id = ?1", [id], |r| r.get(0))
        .optional()?)
}

pub fn id_of_path(conn: &Connection, path: &str) -> Result<Option<i64>> {
    Ok(conn
        .query_row("SELECT id FROM docs WHERE path = ?1", [path], |r| r.get(0))
        .optional()?)
}

#[derive(Serialize)]
pub struct Recent {
    pub id: i64,
    pub title: String,
    pub root: String,
    pub rel: String,
    pub mtime: i64,
}

pub fn recent(conn: &Connection, scopes: Option<&[String]>, limit: usize) -> Result<Vec<Recent>> {
    let filter = match scopes {
        Some(sc) => format!(
            "WHERE scope IN ({})",
            sc.iter().map(|s| format!("'{}'", s.replace('\'', "''"))).chain(std::iter::once("''".into())).collect::<Vec<_>>().join(",")
        ),
        None => String::new(),
    };
    let mut st = conn.prepare(&format!("SELECT id, title, root, rel, mtime FROM docs {filter} ORDER BY mtime DESC LIMIT ?1"))?;
    let rows = st
        .query_map([limit as i64], |r| {
            Ok(Recent {
                id: r.get(0)?,
                title: r.get(1)?,
                root: r.get(2)?,
                rel: r.get(3)?,
                mtime: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn count(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT count(*) FROM docs", [], |r| r.get(0))?)
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
