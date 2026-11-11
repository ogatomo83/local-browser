//! 内容アドレスの置き場 (lb の索引ファイルの隣の `<db>.store/`)。
//!
//! ```text
//! manifest.json          いまの索引の目次 (書き換えるのはこれだけ。一時ファイル → rename で差し替える)
//! objects/b3-…           セグメントと、抽出結果 (本文) の JSON。名前は中身の BLAKE3、書いたら変えない
//! cache/extract/b3-…     元のバイト列のハッシュ + 抽出 wasm のハッシュ → 抽出結果の名前
//! ```
//! この並びのまま静的ホスティングに置けば、モード B のブラウザが差分だけ取りに来られる。

use anyhow::{Context, Result};
use lb_core::manifest::Manifest;
use lb_core::segment::Segment;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub struct Store {
    dir: PathBuf,
}

pub use lb_core::doc::{Extracted, HeadingOut};

impl Store {
    pub fn for_db(db_path: &Path) -> Result<Self> {
        let dir = PathBuf::from(format!("{}.store", db_path.display()));
        std::fs::create_dir_all(dir.join("objects"))?;
        std::fs::create_dir_all(dir.join("cache/extract"))?;
        Ok(Self { dir })
    }

    /// 版が違えば空の manifest を返す (作り直し)
    pub fn load_manifest(&self, index_version: &str) -> Result<Manifest> {
        let p = self.dir.join("manifest.json");
        let m = match std::fs::read(&p) {
            Ok(b) => serde_json::from_slice::<Manifest>(&b).ok(),
            Err(_) => None,
        };
        Ok(match m {
            Some(m) if m.index_version == index_version && m.format == lb_core::manifest::MANIFEST_FORMAT => m,
            Some(m) => {
                eprintln!("[store] 索引の版が変わったので作り直します ({} → {index_version})", m.index_version);
                Manifest::new(index_version)
            }
            None => Manifest::new(index_version),
        })
    }

    pub fn save_manifest(&self, m: &Manifest) -> Result<()> {
        let tmp = self.dir.join("manifest.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(m)?)?;
        std::fs::rename(&tmp, self.dir.join("manifest.json"))?;
        Ok(())
    }

    fn object_path(&self, id: &str) -> PathBuf {
        self.dir.join("objects").join(id)
    }

    /// 置く。同じ名前があれば何もしない (中身が同じなので)
    pub fn put(&self, bytes: &[u8]) -> Result<String> {
        let id = lb_core::hash::content_id(bytes);
        let p = self.object_path(&id);
        if !p.exists() {
            let tmp = p.with_extension("tmp");
            std::fs::write(&tmp, bytes)?;
            std::fs::rename(&tmp, &p)?;
        }
        Ok(id)
    }

    pub fn get(&self, id: &str) -> Result<Vec<u8>> {
        std::fs::read(self.object_path(id)).with_context(|| format!("object がありません: {id}"))
    }

    pub fn has(&self, id: &str) -> bool {
        self.object_path(id).exists()
    }

    pub fn put_extracted(&self, d: &Extracted) -> Result<String> {
        self.put(&d.to_blob().1)
    }

    pub fn get_extracted(&self, id: &str) -> Result<Extracted> {
        Extracted::from_blob(&self.get(id)?).map_err(anyhow::Error::msg)
    }

    pub fn cached_extract(&self, key: &str) -> Option<String> {
        let id = std::fs::read_to_string(self.dir.join("cache/extract").join(key)).ok()?;
        self.has(&id).then_some(id)
    }

    pub fn remember_extract(&self, key: &str, id: &str) -> Result<()> {
        std::fs::write(self.dir.join("cache/extract").join(key), id)?;
        Ok(())
    }

    pub fn load_segments(&self, m: &Manifest) -> Result<Vec<Segment>> {
        m.segments
            .iter()
            .map(|s| Segment::from_bytes(&self.get(&s.id)?).map_err(anyhow::Error::msg))
            .collect()
    }

    /// manifest から辿れない objects と、行き先のなくなったキャッシュを消す
    pub fn gc(&self, keep: &HashSet<String>) -> Result<usize> {
        let mut n = 0;
        for e in std::fs::read_dir(self.dir.join("objects"))? {
            let e = e?;
            let name = e.file_name().to_string_lossy().into_owned();
            if !keep.contains(&name) {
                std::fs::remove_file(e.path())?;
                n += 1;
            }
        }
        for e in std::fs::read_dir(self.dir.join("cache/extract"))? {
            let e = e?;
            let id = std::fs::read_to_string(e.path()).unwrap_or_default();
            if !keep.contains(&id) {
                std::fs::remove_file(e.path())?;
            }
        }
        Ok(n)
    }
}
