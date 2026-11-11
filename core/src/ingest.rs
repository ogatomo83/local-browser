//! 差分取り込みの判断。lb (ディレクトリ / Confluence) とブラウザ (モード C) で同じものを使う。
//!
//! 1. 版 (更新日時+サイズ / 版番号) と閲覧範囲が前回と同じ → 中身を読まない ([`Ingest::check`])
//! 2. 読んだ中身の BLAKE3 が前回と同じ → 版だけ書き換える ([`Ingest::same_content`])
//! 3. 変わっていたら、抽出結果を新しいセグメントに入れる ([`Ingest::add`])
//! 4. 今回見つからなかったもの = 消えた → 削除の印 ([`Ingest::finish`])
//! 5. セグメントが増えすぎたら、生きている文書だけでまとめ直す ([`merge`])
//!
//! 置き場 (ファイル / IndexedDB) への読み書きは呼ぶ側がする。ここは決めるだけ。

use crate::doc::Extracted;
use crate::index::Index;
use crate::manifest::{Manifest, SegmentRef, SourceState};
use crate::segment::{DocEntry, FIELDS, SegmentBuilder};
use serde::Serialize;
use std::collections::HashSet;

/// セグメントがこれより多くなったらまとめ直す
pub const MAX_SEGMENTS: usize = 8;

/// 取り込み元の 1 件
#[derive(Debug, Clone)]
pub struct Source {
    pub key: String,
    pub version: String,
    pub scope: String,
    /// UNIX 秒
    pub mtime: i64,
}

#[derive(Debug, Serialize, Clone, Default)]
pub struct Counts {
    pub scanned: usize,
    pub added: usize,
    pub updated: usize,
    pub unchanged: usize,
    /// 版は変わったが中身のハッシュは同じだった (読みはしたが索引は作り直していない)
    pub same_content: usize,
    pub deleted: usize,
    pub failed: usize,
}

pub struct Ingest {
    manifest: Manifest,
    builder: SegmentBuilder,
    seen: HashSet<String>,
    pub counts: Counts,
}

/// 取り込みの結果。new_segment を置いてから manifest を保存すること
pub struct Finished {
    pub manifest: Manifest,
    pub new_segment: Option<(SegmentRef, Vec<u8>)>,
    pub deleted: Vec<String>,
    pub counts: Counts,
    /// manifest を保存し直す必要がある
    pub dirty: bool,
}

impl Ingest {
    pub fn new(manifest: Manifest) -> Self {
        Self { manifest, builder: SegmentBuilder::new(), seen: HashSet::new(), counts: Counts::default() }
    }

    pub fn previous(&self, key: &str) -> Option<&SourceState> {
        self.manifest.sources.get(key)
    }

    /// 中身を読む必要があるか (版と閲覧範囲が前回と同じなら false)
    pub fn check(&mut self, s: &Source) -> bool {
        self.counts.scanned += 1;
        self.seen.insert(s.key.clone());
        match self.manifest.sources.get(&s.key) {
            Some(p) if p.version == s.version && p.scope == s.scope => {
                self.counts.unchanged += 1;
                false
            }
            _ => true,
        }
    }

    /// 中身のハッシュが前回と同じなら、版だけ書き換えて true
    pub fn same_content(&mut self, s: &Source, hash: &str) -> bool {
        let Some(p) = self.manifest.sources.get_mut(&s.key) else { return false };
        if p.hash != hash || p.scope != s.scope {
            return false;
        }
        p.version = s.version.clone();
        p.mtime = s.mtime;
        self.counts.same_content += 1;
        true
    }

    /// 読めなかった / 抽出できなかった。前回の状態があればそのまま残す (消えた扱いにしない)
    pub fn failed(&mut self) {
        self.counts.failed += 1;
    }

    /// 変わった文書を新しいセグメントに入れる。blob は抽出結果の名前 ([`Extracted::to_blob`])
    pub fn add(&mut self, s: &Source, hash: &str, blob: &str, doc: &Extracted) {
        self.builder.add(
            DocEntry {
                key: s.key.clone(),
                hash: blob.to_string(),
                title: doc.title.clone(),
                scope: s.scope.clone(),
                version: s.version.clone(),
                lens: [0; FIELDS],
            },
            &doc.title,
            &doc.headings_text(),
            &doc.body,
        );
        let prev = self.manifest.sources.insert(
            s.key.clone(),
            SourceState { version: s.version.clone(), hash: hash.to_string(), scope: s.scope.clone(), mtime: s.mtime },
        );
        if prev.is_some() {
            self.counts.updated += 1;
        } else {
            self.counts.added += 1;
        }
    }

    pub fn finish(self) -> Finished {
        let Ingest { mut manifest, builder, seen, mut counts } = self;
        let deleted: Vec<String> = manifest.sources.keys().filter(|k| !seen.contains(*k)).cloned().collect();
        for k in &deleted {
            manifest.sources.remove(k);
        }
        counts.deleted = deleted.len();
        let new_segment = (!builder.is_empty()).then(|| {
            let n = builder.len() as u32;
            let bytes = builder.finish();
            let id = crate::hash::content_id(&bytes);
            (SegmentRef { id, docs: n, bytes: bytes.len() as u64 }, bytes)
        });
        let dirty = new_segment.is_some() || !deleted.is_empty() || counts.same_content > 0;
        if new_segment.is_some() || !deleted.is_empty() {
            manifest.commit(new_segment.as_ref().map(|(r, _)| r.clone()), &deleted);
        }
        Finished { manifest, new_segment, deleted, counts, dirty }
    }
}

/// まとめ直しの結果。segment を置いてから manifest を保存し、それから keep 以外を消す
pub struct Merged {
    pub segment: Option<(SegmentRef, Vec<u8>)>,
    pub keep: HashSet<String>,
}

/// 生きている文書だけで 1 つのセグメントにまとめ直す。ix は manifest から開いたもの
pub fn merge(ix: &Index, manifest: &mut Manifest) -> Merged {
    let parts: Vec<_> = ix.segments().iter().enumerate().map(|(i, s)| (s, ix.live_mask(i))).collect();
    let live = ix.len() as u32;
    let segment = (live > 0).then(|| {
        let bytes = crate::segment::merge(&parts);
        let id = crate::hash::content_id(&bytes);
        (SegmentRef { id, docs: live, bytes: bytes.len() as u64 }, bytes)
    });
    let mut keep: HashSet<String> = ix.live_docs().map(|(_, d)| d.hash.clone()).collect();
    if let Some((r, _)) = &segment {
        keep.insert(r.id.clone());
    }
    manifest.replace_all(segment.as_ref().map(|(r, _)| r.clone()));
    Merged { segment, keep }
}

/// いまの manifest から辿れる objects (セグメントと、生きている文書の抽出結果)
pub fn reachable(ix: &Index, manifest: &Manifest) -> HashSet<String> {
    let mut keep: HashSet<String> = ix.live_docs().map(|(_, d)| d.hash.clone()).collect();
    keep.extend(manifest.segments.iter().map(|s| s.id.clone()));
    keep
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segment::Segment;

    fn src(key: &str, v: &str) -> Source {
        Source { key: key.into(), version: v.into(), scope: "public".into(), mtime: 0 }
    }

    fn doc(body: &str) -> Extracted {
        Extracted { title: body.into(), headings: vec![], body: body.into(), html: String::new() }
    }

    fn open(m: &Manifest, segs: &[(SegmentRef, Vec<u8>)]) -> Index {
        let s = m.segments.iter().map(|r| Segment::from_bytes(&segs.iter().find(|(x, _)| x.id == r.id).unwrap().1).unwrap()).collect();
        Index::new(s, m)
    }

    #[test]
    fn unchanged_touched_changed_deleted() {
        let mut store = Vec::new();
        let mut ing = Ingest::new(Manifest::new("v"));
        for (k, body) in [("a", "障害対応"), ("b", "デプロイ手順")] {
            let s = src(k, "1");
            assert!(ing.check(&s));
            let (blob, _) = doc(body).to_blob();
            ing.add(&s, &format!("h-{body}"), &blob, &doc(body));
        }
        let f = ing.finish();
        assert_eq!((f.counts.added, f.dirty), (2, true));
        store.push(f.new_segment.clone().unwrap());
        let m = f.manifest;

        // a は版だけ変わった (touch)、b は中身が変わった、c は増えた
        let mut ing = Ingest::new(m);
        let a = src("a", "2");
        assert!(ing.check(&a));
        assert!(ing.same_content(&a, "h-障害対応"));
        let b = src("b", "2");
        assert!(ing.check(&b));
        assert!(!ing.same_content(&b, "h-新しい手順"));
        ing.add(&b, "h-新しい手順", "blob-b2", &doc("新しい手順"));
        let f = ing.finish();
        assert_eq!((f.counts.updated, f.counts.same_content, f.counts.deleted), (1, 1, 0));
        store.push(f.new_segment.clone().unwrap());
        let ix = open(&f.manifest, &store);
        assert_eq!(ix.len(), 2);
        let m = f.manifest;

        // 何も変わらなければ書き直さない。b が消えたら削除の印
        let mut ing = Ingest::new(m);
        assert!(!ing.check(&src("a", "2")));
        let f = ing.finish();
        assert_eq!((f.counts.unchanged, f.counts.deleted, f.dirty), (1, 1, true));
        let mut m = f.manifest;
        let ix = open(&m, &store);
        assert_eq!(ix.live_docs().map(|(_, d)| d.key.as_str()).collect::<Vec<_>>(), ["a"]);

        let merged = merge(&ix, &mut m);
        assert_eq!(m.segments.len(), 1);
        assert!(m.tombstones.is_empty());
        assert!(merged.keep.contains(&m.segments[0].id));
    }
}
