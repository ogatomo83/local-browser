//! セグメントの束の上での検索 (FTS5 の代わり)。
//!
//! 語の引き方:
//! - 漢字 1 文字: 1 文字の出現表
//! - 漢字 2 文字以上: 2 文字ずつの出現表を、位置が 1 ずつ並ぶものだけ残す (部分一致と同じ結果)
//! - 英数字・かな: 3 文字以上ならその語を含む語 (sql → postgresql、ベース → データベース)、
//!   2 文字ならその語で始まる語、1 文字なら完全一致だけ。FTS5 (trigram) の部分一致に合わせている
//!
//! 順位は BM25F (タイトル 10 / 見出し 4 / 本文 1)。

use crate::manifest::Manifest;
use crate::segment::{DocEntry, FIELDS, Segment};
use crate::tokenize::{Script, script};
use crate::vocab::Vocab;
use std::collections::{BTreeMap, HashMap};

const WEIGHTS: [f32; FIELDS] = [10.0, 4.0, 1.0];
const K1: f32 = 1.2;
const B: f32 = 0.75;
const PREFIX_LIMIT: usize = 64;
const INFIX_LIMIT: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DocRef {
    pub seg: u32,
    pub doc: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct Scored {
    pub doc: DocRef,
    pub score: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Match {
    /// すべての語を含む
    All,
    /// 中身のある語 (ひらがなだけの語以外) の半分以上を含む
    Loose,
}

pub struct Index {
    segs: Vec<Segment>,
    live: Vec<Vec<bool>>,
    n_live: u32,
    avg_len: [f32; FIELDS],
}

impl Index {
    pub fn new(segs: Vec<Segment>, manifest: &Manifest) -> Self {
        let keys: Vec<Vec<&str>> = segs.iter().map(|s| s.docs.iter().map(|d| d.key.as_str()).collect()).collect();
        let live = manifest.live_masks(&keys);
        let mut n_live = 0;
        let mut sum = [0f64; FIELDS];
        for (s, m) in segs.iter().zip(&live) {
            for (d, &ok) in s.docs.iter().zip(m) {
                if ok {
                    n_live += 1;
                    for f in 0..FIELDS {
                        sum[f] += d.lens[f] as f64;
                    }
                }
            }
        }
        let avg_len = std::array::from_fn(|f| if n_live > 0 { (sum[f] / n_live as f64).max(1.0) as f32 } else { 1.0 });
        Self { segs, live, n_live, avg_len }
    }

    pub fn len(&self) -> usize {
        self.n_live as usize
    }

    pub fn is_empty(&self) -> bool {
        self.n_live == 0
    }

    /// セグメントを取り出す (manifest が変わったあとに開き直すため)
    pub fn into_segments(self) -> Vec<Segment> {
        self.segs
    }

    pub fn segments(&self) -> &[Segment] {
        &self.segs
    }

    pub fn live_mask(&self, seg: usize) -> &[bool] {
        &self.live[seg]
    }

    pub fn doc(&self, r: DocRef) -> &DocEntry {
        &self.segs[r.seg as usize].docs[r.doc as usize]
    }

    pub fn live_docs(&self) -> impl Iterator<Item = (DocRef, &DocEntry)> {
        self.segs.iter().enumerate().flat_map(move |(si, s)| {
            s.docs.iter().enumerate().filter(move |(di, _)| self.live[si][*di]).map(move |(di, d)| {
                (DocRef { seg: si as u32, doc: di as u32 }, d)
            })
        })
    }

    /// 語の一覧 (補正の辞書)。df は生きている文書だけで数える。1 文字の語は入れない
    pub fn vocab(&self) -> Vocab {
        let mut df: BTreeMap<String, u64> = BTreeMap::new();
        for (si, s) in self.segs.iter().enumerate() {
            let live = &self.live[si];
            let all_live = live.iter().all(|&x| x);
            s.for_each_term(|term, n, read| {
                if term.chars().nth(1).is_none() {
                    return;
                }
                let n = if all_live {
                    n
                } else {
                    let mut docs: Vec<u32> = read().into_iter().filter(|p| live[p.doc as usize]).map(|p| p.doc).collect();
                    docs.dedup();
                    docs.len() as u64
                };
                if n > 0 {
                    *df.entry(term.to_string()).or_insert(0) += n;
                }
            });
        }
        Vocab::from_sorted(df).expect("BTreeMap は並んでいる")
    }

    /// 検索。scopes が Some なら、その閲覧範囲の文書だけ
    pub fn search(&self, terms: &[String], scopes: Option<&[String]>, mode: Match, limit: usize, offset: usize) -> (Vec<Scored>, usize) {
        if terms.is_empty() || self.n_live == 0 {
            return (Vec::new(), 0);
        }
        // 語ごとに 文書 → 欄ごとの出現数
        let per_term: Vec<HashMap<DocRef, [u32; FIELDS]>> = terms.iter().map(|t| self.match_term(t, scopes)).collect();
        // ひらがなだけの語 (助詞・送り仮名・「えて」のような活用の切れ端) は、語の一部の一致では数えない。
        // ただしクエリが全部ひらがななら数える
        let hira = |t: &String| t.chars().all(crate::normalize::is_hiragana);
        let all_hira = terms.iter().all(hira);
        let content: Vec<bool> = terms.iter().map(|t| all_hira || !hira(t)).collect();
        let need = match mode {
            Match::All => terms.len(),
            Match::Loose => content.iter().filter(|&&c| c).count().div_ceil(2).max(1),
        };
        let mut hits: HashMap<DocRef, (usize, f32)> = HashMap::new();
        for (ti, m) in per_term.iter().enumerate() {
            // 語の一部の一致では、ひらがなだけの語は数えも点数もしない
            if mode == Match::Loose && !content[ti] {
                continue;
            }
            let df = m.len() as f32;
            let idf = (1.0 + (self.n_live as f32 - df + 0.5) / (df + 0.5)).ln();
            for (r, tf) in m {
                let d = self.doc(*r);
                let mut tfw = 0.0;
                for f in 0..FIELDS {
                    let norm = 1.0 - B + B * d.lens[f] as f32 / self.avg_len[f];
                    tfw += WEIGHTS[f] * tf[f] as f32 / norm;
                }
                let e = hits.entry(*r).or_insert((0, 0.0));
                e.0 += 1;
                e.1 += idf * tfw * (K1 + 1.0) / (tfw + K1);
            }
        }
        // 語の一部の一致では、一致した語の割合を掛ける (多くの語が合う文書を上に)
        let n_content = content.iter().filter(|&&c| c).count().max(1) as f32;
        let mut out: Vec<Scored> = hits
            .into_iter()
            .filter(|(_, (n, _))| *n >= need)
            .map(|(doc, (n, score))| {
                let coord = if mode == Match::Loose { (n as f32 / n_content).sqrt() } else { 1.0 };
                Scored { doc, score: score * coord }
            })
            .collect();
        out.sort_by(|a, b| {
            b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal).then_with(|| self.doc(a.doc).key.cmp(&self.doc(b.doc).key))
        });
        let total = out.len();
        (out.into_iter().skip(offset).take(limit).collect(), total)
    }

    fn match_term(&self, term: &str, scopes: Option<&[String]>) -> HashMap<DocRef, [u32; FIELDS]> {
        let mut out: HashMap<DocRef, [u32; FIELDS]> = HashMap::new();
        let cs: Vec<char> = term.chars().collect();
        let Some(first) = cs.first() else { return out };
        for (si, seg) in self.segs.iter().enumerate() {
            let ok = |doc: u32| {
                self.live[si][doc as usize]
                    && scopes.is_none_or(|sc| sc.iter().any(|s| *s == seg.docs[doc as usize].scope))
            };
            let mut add = |doc: u32, field: u8, n: u32| {
                if ok(doc) {
                    out.entry(DocRef { seg: si as u32, doc }).or_insert([0; FIELDS])[field as usize] += n;
                }
            };
            if script(*first) == Some(Script::Kanji) && cs.len() >= 2 {
                // 2 文字ずつの出現位置が 1 ずつ並ぶところ
                let grams: Vec<String> = cs.windows(2).map(|w| w.iter().collect()).collect();
                let mut cur: HashMap<(u32, u8), Vec<u32>> =
                    seg.postings(&grams[0]).into_iter().map(|p| ((p.doc, p.field), p.positions)).collect();
                for (j, g) in grams.iter().enumerate().skip(1) {
                    let next: HashMap<(u32, u8), Vec<u32>> =
                        seg.postings(g).into_iter().map(|p| ((p.doc, p.field), p.positions)).collect();
                    cur.retain(|k, ps| {
                        let Some(np) = next.get(k) else { return false };
                        ps.retain(|p| np.binary_search(&(p + j as u32)).is_ok());
                        !ps.is_empty()
                    });
                }
                for ((doc, field), ps) in cur {
                    add(doc, field, ps.len() as u32);
                }
            } else {
                let kanji = script(*first) == Some(Script::Kanji);
                let keys = if cs.len() >= 3 && !kanji {
                    seg.terms_containing(term, INFIX_LIMIT)
                } else if cs.len() == 2 && !kanji {
                    seg.terms_with_prefix(term, PREFIX_LIMIT)
                } else {
                    vec![term.to_string()]
                };
                for k in keys {
                    for p in seg.postings(&k) {
                        add(p.doc, p.field, p.positions.len() as u32);
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::SegmentRef;
    use crate::segment::SegmentBuilder;

    fn entry(key: &str, scope: &str) -> DocEntry {
        DocEntry { key: key.into(), hash: String::new(), title: key.into(), scope: scope.into(), version: "1".into(), lens: [0; FIELDS] }
    }

    fn index() -> Index {
        let mut b = SegmentBuilder::new();
        b.add(entry("deploy", "public"), "デプロイ手順", "ロールバック", "本番へのデプロイは平日に。PostgreSQL を使う");
        b.add(entry("incident", "public"), "障害対応フロー", "最初の 15 分", "障害が起きたら当番に連絡");
        b.add(entry("secret", "team-dev"), "障害の振り返り", "", "社外秘の障害対応メモ");
        let s = Segment::from_bytes(&b.finish()).unwrap();
        let mut m = Manifest::new("v");
        m.commit(Some(SegmentRef { id: "s".into(), docs: 3, bytes: 0 }), &[]);
        Index::new(vec![s], &m)
    }

    fn keys(ix: &Index, terms: &[&str], scopes: Option<&[String]>, mode: Match) -> Vec<String> {
        let terms: Vec<String> = terms.iter().map(|s| s.to_string()).collect();
        ix.search(&terms, scopes, mode, 10, 0).0.iter().map(|h| ix.doc(h.doc).key.clone()).collect()
    }

    #[test]
    fn phrase_and_prefix() {
        let ix = index();
        assert_eq!(keys(&ix, &["障害対応"], None, Match::All), ["incident", "secret"]);
        assert_eq!(keys(&ix, &["害対"], None, Match::All), ["incident", "secret"]);
        assert_eq!(keys(&ix, &["postgres"], None, Match::All), ["deploy"]);
        assert_eq!(keys(&ix, &["デプロイ", "手順"], None, Match::All), ["deploy"]);
        assert!(keys(&ix, &["対障"], None, Match::All).is_empty());
    }

    #[test]
    fn scopes_filter() {
        let ix = index();
        let public = ["public".to_string()];
        assert_eq!(keys(&ix, &["障害"], Some(&public), Match::All), ["incident"]);
    }

    #[test]
    fn vocab_counts_live_docs() {
        let ix = index();
        let v = ix.vocab();
        assert_eq!(v.df("障害"), 2);
        assert_eq!(v.df("デプロイ"), 1);
        assert_eq!(v.df("障"), 0);
    }
}
