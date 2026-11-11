//! manifest: いまの索引がどのセグメントでできているかの目次。
//!
//! 書き換えるのはこれだけ。セグメントと本文は不変で、名前は中身の BLAKE3。
//! ブラウザ (モード B) は manifest を取ってきて、持っていないセグメントだけ取りに行く。
//! モード C では、このほかに sources (ファイルごとの版と中身のハッシュ) を見て、
//! リロードのときに変わったファイルだけ読み直す。

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

pub const MANIFEST_FORMAT: u32 = 1;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    /// 索引の作り方の版 (形式 / 正規化の規則 / 抽出 wasm のハッシュ)。違えば作り直す
    pub index_version: String,
    /// 書き換えるたびに 1 増える
    pub generation: u64,
    /// 古い順
    pub segments: Vec<SegmentRef>,
    pub tombstones: Vec<Tombstone>,
    /// 出どころ (ファイル / Confluence ページ) ごとの状態
    pub sources: BTreeMap<String, SourceState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentRef {
    pub id: String,
    pub docs: u32,
    pub bytes: u64,
}

/// key の文書を、segments[..before] の中では無いものとして扱う (削除の印)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tombstone {
    pub key: String,
    pub before: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceState {
    /// 出どころでの版。ファイルは "更新日時-サイズ" (最初の足切り用)、Confluence は版番号
    pub version: String,
    /// 元のバイト列の BLAKE3 (本当に変わったかの判定はこちら)
    pub hash: String,
    pub scope: String,
    /// 出どころでの更新日時 (UNIX 秒)。「最近更新された文書」用
    #[serde(default)]
    pub mtime: i64,
}

impl Manifest {
    pub fn new(index_version: &str) -> Self {
        Self { format: MANIFEST_FORMAT, index_version: index_version.into(), ..Default::default() }
    }

    /// 変更を 1 回分まとめて反映する。updated は new_segment に入っている文書
    /// (古いものは新しいセグメントに隠れる)。deleted には削除の印を付ける。
    pub fn commit(&mut self, new_segment: Option<SegmentRef>, deleted: &[String]) {
        let before = self.segments.len() as u32;
        for k in deleted {
            self.tombstones.push(Tombstone { key: k.clone(), before });
        }
        if let Some(s) = new_segment {
            self.segments.push(s);
        }
        self.generation += 1;
    }

    /// セグメントごとの「生きている文書」の印。docs[i] はセグメント i の文書の key
    pub fn live_masks(&self, docs: &[Vec<&str>]) -> Vec<Vec<bool>> {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut masks = vec![Vec::new(); docs.len()];
        for i in (0..docs.len()).rev() {
            masks[i] = docs[i]
                .iter()
                .map(|&k| {
                    let dead = seen.contains(k)
                        || self.tombstones.iter().any(|t| t.key == k && (i as u32) < t.before);
                    seen.insert(k);
                    !dead
                })
                .collect();
        }
        masks
    }

    /// セグメントが増えすぎた、または削除の印が多すぎるならまとめ直す
    pub fn needs_merge(&self, max_segments: usize) -> bool {
        let docs: u32 = self.segments.iter().map(|s| s.docs).sum();
        self.segments.len() > max_segments || (docs > 0 && self.tombstones.len() as u32 * 4 > docs)
    }

    /// まとめ直したあと: セグメントは 1 つ、削除の印はなし
    pub fn replace_all(&mut self, merged: Option<SegmentRef>) {
        self.segments = merged.into_iter().collect();
        self.tombstones.clear();
        self.generation += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(id: &str) -> SegmentRef {
        SegmentRef { id: id.into(), docs: 1, bytes: 0 }
    }

    #[test]
    fn newer_hides_older_and_tombstones() {
        let mut m = Manifest::new("v");
        m.commit(Some(seg("s0")), &[]);
        m.commit(Some(seg("s1")), &["c".into()]);
        m.commit(None, &["b".into()]);
        let docs = vec![vec!["a", "b", "c"], vec!["a"]];
        let live = m.live_masks(&docs);
        assert_eq!(live, vec![vec![false, false, false], vec![true]]);
        // 消したあとに足し直したものは見える
        m.commit(Some(seg("s2")), &[]);
        let docs = vec![vec!["a", "b", "c"], vec!["a"], vec!["b"]];
        assert_eq!(m.live_masks(&docs)[2], vec![true]);
        assert_eq!(m.generation, 4);
    }
}
