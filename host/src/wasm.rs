//! 抽出用 wasm コンポーネントのホスト側。
//! コンポーネントにはファイル・ネットワーク・環境変数を一切与えない (空の WasiCtx)。
//! さらに燃料 (命令数) とメモリに上限を付け、壊れた入力で暴走しても止まるようにする。

use anyhow::{Result, anyhow};
use std::path::Path;
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

wasmtime::component::bindgen!({
    world: "extractor",
    path: "../wit",
});

pub use exports::local_browser::extractor::extract::Document;

const FUEL_PER_DOC: u64 = 5_000_000_000;
const MEMORY_LIMIT: usize = 512 << 20;

struct State {
    wasi: WasiCtx,
    table: ResourceTable,
    limits: StoreLimits,
}

impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

pub struct ExtractorHost {
    engine: Engine,
    pre: ExtractorPre<State>,
    extensions: Vec<String>,
    /// wasm ファイルの BLAKE3。抽出結果のキャッシュのキーに入れる (抽出の仕方が変われば作り直す)
    pub id: String,
}

impl ExtractorHost {
    pub fn load(path: &Path) -> Result<Self> {
        let mut config = Config::new();
        config.consume_fuel(true);
        let engine = Engine::new(&config)?;
        let bytes = std::fs::read(path).map_err(|e| anyhow!("wasm を読めません: {}: {e}", path.display()))?;
        let id = lb_core::hash::content_id(&bytes);
        let component = Component::from_binary(&engine, &bytes)
            .map_err(|e| anyhow!("wasm コンポーネントを読めません: {}: {e:#}", path.display()))?;
        let mut linker = Linker::<State>::new(&engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
        let pre = ExtractorPre::new(linker.instantiate_pre(&component)?)?;
        let mut host = Self {
            engine,
            pre,
            extensions: Vec::new(),
            id,
        };
        let (bindings, mut store) = host.instantiate()?;
        host.extensions = bindings
            .local_browser_extractor_extract()
            .call_supported_extensions(&mut store)?;
        Ok(host)
    }

    pub fn extensions(&self) -> &[String] {
        &self.extensions
    }

    fn instantiate(&self) -> Result<(Extractor, Store<State>)> {
        let state = State {
            wasi: WasiCtx::builder().build(), // 権限ゼロ
            table: ResourceTable::new(),
            limits: StoreLimitsBuilder::new().memory_size(MEMORY_LIMIT).build(),
        };
        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| &mut s.limits);
        store.set_fuel(FUEL_PER_DOC)?;
        let bindings = self.pre.instantiate(&mut store)?;
        Ok((bindings, store))
    }

    /// 1 文書ごとに新しいインスタンスを作る (状態が文書間で漏れない)
    pub fn extract(&self, path: &str, content: &[u8]) -> Result<Document> {
        let (bindings, mut store) = self.instantiate()?;
        bindings
            .local_browser_extractor_extract()
            .call_extract(&mut store, path, content)?
            .map_err(|e| anyhow!(e))
    }
}
