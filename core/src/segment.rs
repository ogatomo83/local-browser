//! 不変セグメント。いったん書いたら変えない索引の部品。
//!
//! 変更は「新しいセグメントを足す + 古い文書に削除の印を付ける」で表し、
//! ときどき生きている文書だけでまとめ直す (Lucene と同じ考え方)。
//! 名前は中身の BLAKE3 なので、同じ中身なら同じ名前になり、ブラウザは
//! 持っていないセグメントだけ取ってくればよい。
//!
//! 形式 (すべて little endian / varint):
//! ```text
//! "LBSEG" 1 | 文書数 | 文書 × n | 語の一覧 (fst: 語 → 出現表の位置) | 出現表
//! 出現表: 文書数 (df) | 件数 | (文書の差分, 欄, 位置の数, 位置の差分…) × 件数
//! ```
//! 欄は 0 = タイトル, 1 = 見出し, 2 = 本文。位置は正規化済み文字列の中の文字位置。

use crate::normalize::normalize;
use crate::tokenize::{Script, tokens};
use fst::{IntoStreamer, Map, MapBuilder, Streamer};
use std::collections::BTreeMap;

const MAGIC: &[u8] = b"LBSEG\x01";
pub const FIELDS: usize = 3;

/// セグメントの中の 1 文書
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocEntry {
    /// 出どころでの名前 (file:/abs/path, confluence:DEV/123)
    pub key: String,
    /// 抽出結果 (タイトル・見出し・本文・HTML) の BLAKE3。本文はこの名前で別に置く
    pub hash: String,
    pub title: String,
    /// 閲覧範囲 (public, team-dev …)
    pub scope: String,
    /// 出どころでの版 (ファイルなら更新日時とサイズ、Confluence なら版番号)
    pub version: String,
    /// 欄ごとの長さ (文字数)
    pub lens: [u32; FIELDS],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Posting {
    pub doc: u32,
    pub field: u8,
    pub positions: Vec<u32>,
}

// ---------------------------------------------------------------- 書く

#[derive(Default)]
pub struct SegmentBuilder {
    docs: Vec<DocEntry>,
    /// 語 → (文書, 欄) → 位置
    postings: BTreeMap<String, BTreeMap<(u32, u8), Vec<u32>>>,
}

impl SegmentBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    /// 文書を足す。欄の文字列は正規化前のものを渡す
    pub fn add(&mut self, mut entry: DocEntry, title: &str, headings: &str, body: &str) {
        let doc = self.docs.len() as u32;
        for (f, text) in [title, headings, body].into_iter().enumerate() {
            let norm = normalize(text);
            entry.lens[f] = norm.chars().count() as u32;
            for (term, pos) in field_terms(&norm) {
                self.postings.entry(term).or_default().entry((doc, f as u8)).or_default().push(pos);
            }
        }
        self.docs.push(entry);
    }

    pub fn finish(self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        put_var(&mut out, self.docs.len() as u64);
        for d in &self.docs {
            for s in [&d.key, &d.hash, &d.title, &d.scope, &d.version] {
                put_str(&mut out, s);
            }
            for l in d.lens {
                put_var(&mut out, l as u64);
            }
        }
        let mut post = Vec::new();
        let mut fst = MapBuilder::memory();
        for (term, entries) in &self.postings {
            fst.insert(term, post.len() as u64).expect("BTreeMap は並んでいる");
            let mut docs: Vec<u32> = entries.keys().map(|k| k.0).collect();
            docs.dedup();
            put_var(&mut post, docs.len() as u64);
            put_var(&mut post, entries.len() as u64);
            let mut prev_doc = 0;
            for ((doc, field), positions) in entries {
                put_var(&mut post, (doc - prev_doc) as u64);
                prev_doc = *doc;
                post.push(*field);
                let mut ps = positions.clone();
                ps.sort_unstable();
                ps.dedup();
                put_var(&mut post, ps.len() as u64);
                let mut prev = 0;
                for p in ps {
                    put_var(&mut post, (p - prev) as u64);
                    prev = p;
                }
            }
        }
        let fst = fst.into_inner().expect("メモリへの書き込み");
        put_var(&mut out, fst.len() as u64);
        out.extend_from_slice(&fst);
        put_var(&mut out, post.len() as u64);
        out.extend_from_slice(&post);
        out
    }
}

/// 欄の中の語と位置。
/// - 英数字・かな: 語そのもの
/// - 漢字: 1 文字ずつ、2 文字ずつ、40 文字以下なら並び全体 (語の一覧と補正のため)
fn field_terms(norm: &str) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    for t in tokens(norm) {
        let start = t.start as u32;
        if t.script == Script::Kanji {
            let cs: Vec<char> = t.text.chars().collect();
            for (i, c) in cs.iter().enumerate() {
                out.push((c.to_string(), start + i as u32));
            }
            for (i, w) in cs.windows(2).enumerate() {
                out.push((w.iter().collect(), start + i as u32));
            }
            if t.len >= 3 && t.len <= 40 {
                out.push((t.text, start));
            }
        } else if t.len <= 40 {
            out.push((t.text, start));
        }
    }
    out
}

// ---------------------------------------------------------------- 読む

pub struct Segment {
    pub docs: Vec<DocEntry>,
    dict: Map<Vec<u8>>,
    post: Vec<u8>,
}

impl Segment {
    pub fn from_bytes(b: &[u8]) -> Result<Self, String> {
        let mut r = Reader { b, i: 0 };
        if r.take(MAGIC.len())? != MAGIC {
            return Err("セグメントの形式が違います".into());
        }
        let n = r.var()? as usize;
        let mut docs = Vec::with_capacity(n);
        for _ in 0..n {
            let (key, hash, title, scope, version) = (r.str()?, r.str()?, r.str()?, r.str()?, r.str()?);
            let lens = [r.var()? as u32, r.var()? as u32, r.var()? as u32];
            docs.push(DocEntry { key, hash, title, scope, version, lens });
        }
        let fl = r.var()? as usize;
        let dict = Map::new(r.take(fl)?.to_vec()).map_err(|e| e.to_string())?;
        let pl = r.var()? as usize;
        let post = r.take(pl)?.to_vec();
        Ok(Self { docs, dict, post })
    }

    /// 語の出現表。語がなければ空
    pub fn postings(&self, term: &str) -> Vec<Posting> {
        self.dict.get(term).map(|off| self.read_postings(off as usize)).unwrap_or_default()
    }

    /// 語を含む文書の数 (削除の印は考えない)
    pub fn df(&self, term: &str) -> u64 {
        self.dict
            .get(term)
            .and_then(|off| Reader { b: &self.post, i: off as usize }.var().ok())
            .unwrap_or(0)
    }

    /// prefix で始まる語 (prefix 自身を含む)。多すぎるときは limit で打ち切る
    pub fn terms_with_prefix(&self, prefix: &str, limit: usize) -> Vec<String> {
        let mut out = Vec::new();
        let mut s = self.dict.range().ge(prefix).into_stream();
        while let Some((k, _)) = s.next() {
            if !k.starts_with(prefix.as_bytes()) || out.len() >= limit {
                break;
            }
            if let Ok(k) = std::str::from_utf8(k) {
                out.push(k.to_string());
            }
        }
        out
    }

    /// sub を含む語 (語の途中の一致。データベース ← ベース)。辞書を全部なめるので limit で打ち切る
    pub fn terms_containing(&self, sub: &str, limit: usize) -> Vec<String> {
        let needle = sub.as_bytes();
        let mut out = Vec::new();
        let mut s = self.dict.stream();
        while let Some((k, _)) = s.next() {
            if k.len() >= needle.len() && k.windows(needle.len()).any(|w| w == needle) {
                if let Ok(k) = std::str::from_utf8(k) {
                    out.push(k.to_string());
                    if out.len() >= limit {
                        break;
                    }
                }
            }
        }
        out
    }

    /// 全部の語と df (語の一覧を作るため)
    pub fn for_each_term(&self, mut f: impl FnMut(&str, u64, &dyn Fn() -> Vec<Posting>)) {
        let mut s = self.dict.stream();
        while let Some((k, off)) = s.next() {
            let Ok(k) = std::str::from_utf8(k) else { continue };
            let df = Reader { b: &self.post, i: off as usize }.var().unwrap_or(0);
            f(k, df, &|| self.read_postings(off as usize));
        }
    }

    fn read_postings(&self, off: usize) -> Vec<Posting> {
        let mut r = Reader { b: &self.post, i: off };
        let mut out = Vec::new();
        let parse = |r: &mut Reader, out: &mut Vec<Posting>| -> Result<(), String> {
            let _df = r.var()?;
            let n = r.var()?;
            let mut doc = 0u32;
            for _ in 0..n {
                doc += r.var()? as u32;
                let field = r.take(1)?[0];
                let np = r.var()?;
                let mut positions = Vec::with_capacity(np as usize);
                let mut p = 0u32;
                for _ in 0..np {
                    p += r.var()? as u32;
                    positions.push(p);
                }
                out.push(Posting { doc, field, positions });
            }
            Ok(())
        };
        let _ = parse(&mut r, &mut out);
        out
    }
}

/// いくつかのセグメントの生きている文書だけで、新しいセグメントを作る (まとめ直し)
pub fn merge(parts: &[(&Segment, &[bool])]) -> Vec<u8> {
    let mut b = SegmentBuilder::new();
    let mut remap: Vec<Vec<Option<u32>>> = Vec::new();
    for (seg, live) in parts {
        let mut m = vec![None; seg.docs.len()];
        for (i, d) in seg.docs.iter().enumerate() {
            if live.get(i).copied().unwrap_or(false) {
                m[i] = Some(b.docs.len() as u32);
                b.docs.push(d.clone());
            }
        }
        remap.push(m);
    }
    for ((seg, _), m) in parts.iter().zip(&remap) {
        seg.for_each_term(|term, _df, read| {
            for p in read() {
                if let Some(nd) = m[p.doc as usize] {
                    b.postings
                        .entry(term.to_string())
                        .or_default()
                        .entry((nd, p.field))
                        .or_default()
                        .extend(p.positions);
                }
            }
        });
    }
    b.finish()
}

// ---------------------------------------------------------------- varint

fn put_var(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    put_var(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.i.checked_add(n).filter(|&e| e <= self.b.len()).ok_or("セグメントが途中で切れています")?;
        let s = &self.b[self.i..end];
        self.i = end;
        Ok(s)
    }

    fn var(&mut self) -> Result<u64, String> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = self.take(1)?[0];
            v |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err("varint が長すぎます".into())
    }

    fn str(&mut self) -> Result<String, String> {
        let n = self.var()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn entry(key: &str) -> DocEntry {
        DocEntry {
            key: key.into(),
            hash: format!("h-{key}"),
            title: key.into(),
            scope: "public".into(),
            version: "1".into(),
            lens: [0; FIELDS],
        }
    }

    #[test]
    fn roundtrip() {
        let mut b = SegmentBuilder::new();
        b.add(entry("a"), "デプロイ手順", "", "本番へのデプロイ。障害対応も");
        b.add(entry("b"), "障害対応フロー", "最初の 15 分", "K8s の障害");
        let seg = Segment::from_bytes(&b.finish()).unwrap();
        assert_eq!(seg.docs.len(), 2);
        assert_eq!(seg.docs[1].lens[0], 6); // 障害対応フロ (末尾の長音は落ちる)
        assert_eq!(seg.df("障害"), 2);
        assert_eq!(seg.df("k8s"), 1);
        assert_eq!(seg.df("デプロイ"), 1);
        let p = seg.postings("障害");
        assert_eq!(p[0], Posting { doc: 0, field: 2, positions: vec![9] });
        assert_eq!(seg.terms_with_prefix("障", 10), ["障", "障害", "障害対応"]);
        assert_eq!(seg.terms_containing("プロ", 10), ["デプロイ"]);
    }

    #[test]
    fn merge_keeps_live_docs() {
        let mut b = SegmentBuilder::new();
        b.add(entry("a"), "古い手順", "", "ロールバック");
        b.add(entry("b"), "残る", "", "キャッシュ");
        let s1 = Segment::from_bytes(&b.finish()).unwrap();
        let mut b = SegmentBuilder::new();
        b.add(entry("a"), "新しい手順", "", "ロールバック");
        let s2 = Segment::from_bytes(&b.finish()).unwrap();
        let m = Segment::from_bytes(&merge(&[(&s1, &[false, true]), (&s2, &[true])])).unwrap();
        let keys: Vec<_> = m.docs.iter().map(|d| d.key.as_str()).collect();
        assert_eq!(keys, ["b", "a"]);
        assert_eq!(m.df("ロールバック"), 1);
        assert_eq!(m.postings("ロールバック")[0].doc, 1);
        assert_eq!(m.df("古"), 0);
    }
}
