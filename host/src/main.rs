mod db;
mod eval;
mod indexer;
mod search;
mod server;
mod source;
mod store;
mod wasm;

use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

const USAGE: &str = "\
lb — ローカル文書をブラウザで検索する

使い方:
  lb [オプション] <ディレクトリ>...
  lb eval <クエリ集.json> [オプション] <ディレクトリ>...   検索の質を確かめる (一時的な索引を作る)
  lb index [オプション] <ディレクトリ>...                  索引を 1 回だけ更新して、結果を JSON で出す

オプション:
  --port <N>          待ち受けポート (既定: 7878)
  --db <PATH>         索引ファイル (既定: ~/.local-browser/index.db)
  --extractor <PATH>  抽出用 wasm (既定: 実行ファイルの隣 → target/wasm32-wasip2/release/extractor.wasm)
  --interval <秒>     差分更新の間隔 (既定: 30、0 で無効)
  --engine <名前>     文書を引く仕組み: segments (自前の索引、既定) / fts5 (SQLite FTS5)
  --confluence <PATH> Confluence の模擬データ (JSON) も取り込む (複数可)
  --scope <名前>      見てよい閲覧範囲 (複数可。省略すると全部)
  -h, --help          このヘルプ
";

struct Args {
    eval: Option<PathBuf>,
    index_once: bool,
    port: u16,
    db: PathBuf,
    extractor: PathBuf,
    interval: u64,
    roots: Vec<PathBuf>,
    engine: search::Engine,
    confluence: Vec<PathBuf>,
    scopes: Option<Vec<String>>,
}

fn parse_args() -> Result<Args> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let mut a = Args {
        eval: None,
        index_once: false,
        port: 7878,
        db: home.join(".local-browser/index.db"),
        extractor: default_extractor(),
        interval: 30,
        roots: Vec::new(),
        engine: search::Engine::Segments,
        confluence: Vec::new(),
        scopes: None,
    };
    let mut it = std::env::args().skip(1).peekable();
    if it.peek().map(|s| s == "index").unwrap_or(false) {
        it.next();
        a.index_once = true;
    } else if it.peek().map(|s| s == "eval").unwrap_or(false) {
        it.next();
        a.eval = Some(it.next().context("eval にクエリ集のパスがありません")?.into());
        a.db = std::env::temp_dir().join(format!("lb-eval-{}.db", std::process::id()));
    }
    while let Some(arg) = it.next() {
        let mut val = |name: &str| it.next().with_context(|| format!("{name} に値がありません"));
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "--port" => a.port = val("--port")?.parse()?,
            "--db" => a.db = val("--db")?.into(),
            "--extractor" => a.extractor = val("--extractor")?.into(),
            "--interval" => a.interval = val("--interval")?.parse()?,
            "--engine" => a.engine = val("--engine")?.parse()?,
            "--confluence" => a.confluence.push(val("--confluence")?.into()),
            "--scope" => a.scopes.get_or_insert_with(Vec::new).push(val("--scope")?),
            s if s.starts_with('-') => bail!("不明なオプション: {s}\n\n{USAGE}"),
            s => a.roots.push(PathBuf::from(s)),
        }
    }
    if a.roots.is_empty() && a.confluence.is_empty() {
        bail!("ディレクトリを 1 つ以上指定してください\n\n{USAGE}");
    }
    a.roots = a
        .roots
        .iter()
        .map(|r| r.canonicalize().with_context(|| format!("見つかりません: {}", r.display())))
        .collect::<Result<_>>()?;
    Ok(a)
}

fn default_extractor() -> PathBuf {
    if let Some(p) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("extractor.wasm")))
        .filter(|p| p.exists())
    {
        return p;
    }
    PathBuf::from("target/wasm32-wasip2/release/extractor.wasm")
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e:#}");
            std::process::exit(2);
        }
    };

    let t = std::time::Instant::now();
    let extractor = Arc::new(ExtractorHostLoader::load(&args.extractor)?);
    eprintln!(
        "[wasm] {} を読み込みました ({}ms) 対応: {}",
        args.extractor.display(),
        t.elapsed().as_millis(),
        extractor.extensions().join(", ")
    );

    let sources = indexer::Sources { dirs: args.roots.clone(), confluence: args.confluence.clone() };
    if args.index_once {
        let rep = indexer::run(&args.db, &sources, &extractor)?;
        println!("{}", serde_json::to_string(&rep)?);
        return Ok(());
    }
    if let Some(golden) = &args.eval {
        let ok = eval::run(golden, &args.db, &sources, &extractor, args.engine, args.scopes.as_deref())?;
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", args.db.display()));
        }
        let _ = std::fs::remove_dir_all(format!("{}.store", args.db.display()));
        std::process::exit(if ok { 0 } else { 1 });
    }

    let conn = db::open(&args.db)?;
    let searcher = search::Searcher::load(args.engine, &conn, indexer::load_index(&args.db, &extractor)?)?;
    let app = Arc::new(server::App {
        port: args.port,
        read: Mutex::new(conn),
        sources,
        engine: args.engine,
        scopes: args.scopes.clone(),
        db_path: args.db.clone(),
        roots: args.roots.clone(),
        extractor,
        indexing: AtomicBool::new(false),
        last: Mutex::new(None),
        searcher: RwLock::new(Arc::new(searcher)),
    });

    for r in &args.roots {
        eprintln!("[root] {}", r.display());
    }
    for c in &args.confluence {
        eprintln!("[confluence] {} (模擬データ)", c.display());
    }
    eprintln!("[search] engine = {:?}, 閲覧範囲 = {}", args.engine, args.scopes.as_ref().map(|s| s.join(", ")).unwrap_or("すべて".into()));
    server::start_index(app.clone());
    if args.interval > 0 {
        let app = app.clone();
        let every = Duration::from_secs(args.interval);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                server::start_index(app.clone());
            }
        });
    }

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], args.port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("{addr} で待ち受けできません"))?;
    eprintln!("[http] http://localhost:{}/ を開いてください", args.port);
    axum::serve(listener, server::router(app)).await?;
    Ok(())
}

use wasm::ExtractorHost as ExtractorHostLoader;
