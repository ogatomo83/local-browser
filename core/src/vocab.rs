//! 語の一覧 (語 → その語を含む文書数 df)。fst で持つ。
//!
//! 打ち間違いの補正は、文書ではなくこの一覧に対して行う。
//!
//! fst 0.4 の Levenshtein オートマトンは、マルチバイト文字で候補を取りこぼす
//! (「検策」から「検索」が出てこない。2026-11-11 に確認)。そのため候補探しは、
//! 長さで絞った全件走査 + 打ち切り付きの編集距離で行う。語が数十万を超えて
//! 遅くなったら、削除近傍 (SymSpell 方式) の表に置き換える。

use fst::{Map, MapBuilder, Streamer};

pub struct Vocab {
    map: Map<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub term: String,
    pub df: u64,
    pub dist: u32,
}

impl Default for Vocab {
    fn default() -> Self {
        Self::empty()
    }
}

impl Vocab {
    pub fn empty() -> Self {
        Self { map: Map::default() }
    }

    /// バイト順に並んだ (語, df) から作る。順序が崩れていたら Err。
    pub fn from_sorted<I, S>(items: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = (S, u64)>,
        S: AsRef<[u8]>,
    {
        let mut b = MapBuilder::memory();
        for (k, v) in items {
            b.insert(k, v).map_err(|e| e.to_string())?;
        }
        let bytes = b.into_inner().map_err(|e| e.to_string())?;
        let map = Map::new(bytes).map_err(|e| e.to_string())?;
        Ok(Self { map })
    }

    /// 並んでいない (語, df) から作る。同じ語は df を足す。
    pub fn from_unsorted<I: IntoIterator<Item = (String, u64)>>(items: I) -> Self {
        let mut m = std::collections::BTreeMap::new();
        for (k, v) in items {
            *m.entry(k).or_insert(0) += v;
        }
        Self::from_sorted(m).expect("BTreeMap は並んでいる")
    }

    /// fst のバイト列 (ブラウザへ配るときはこれをそのまま置く)
    pub fn as_bytes(&self) -> &[u8] {
        self.map.as_fst().as_bytes()
    }

    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, String> {
        Ok(Self { map: Map::new(bytes).map_err(|e| e.to_string())? })
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn df(&self, term: &str) -> u64 {
        self.map.get(term).unwrap_or(0)
    }

    /// 編集距離 max_dist 以内の語 (自分自身は除く)。距離が近い順、同じなら df が多い順。
    pub fn candidates(&self, term: &str, max_dist: u32) -> Vec<Candidate> {
        let tc: Vec<char> = term.chars().collect();
        let mut kc: Vec<char> = Vec::new();
        let mut cands = Vec::new();
        let mut s = self.map.stream();
        while let Some((k, df)) = s.next() {
            let Ok(k) = std::str::from_utf8(k) else { continue };
            kc.clear();
            kc.extend(k.chars());
            if kc.len().abs_diff(tc.len()) > max_dist as usize || kc == tc {
                continue;
            }
            if let Some(d) = bounded_levenshtein(&tc, &kc, max_dist) {
                cands.push(Candidate { term: k.to_string(), df, dist: d });
            }
        }
        cands.sort_by(|a, b| a.dist.cmp(&b.dist).then(b.df.cmp(&a.df)).then(a.term.cmp(&b.term)));
        cands
    }
}

/// 編集距離。max を超えると分かった時点で None
pub fn bounded_levenshtein(a: &[char], b: &[char], max: u32) -> Option<u32> {
    let mut prev: Vec<u32> = (0..=b.len() as u32).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i as u32 + 1;
        let mut row_min = cur[0];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + u32::from(ca != cb);
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
            row_min = row_min.min(cur[j + 1]);
        }
        if row_min > max {
            return None;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    (prev[b.len()] <= max).then_some(prev[b.len()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_near() {
        let v = Vocab::from_unsorted([("k8s".into(), 5), ("k9s".into(), 1), ("kubernetes".into(), 7)]);
        let c = v.candidates("k8se", 1);
        assert_eq!(c[0].term, "k8s");
        assert_eq!(v.df("kubernetes"), 7);
        assert_eq!(v.candidates("kubernets", 2)[0].term, "kubernetes");
        let round = Vocab::from_bytes(v.as_bytes().to_vec()).unwrap();
        assert_eq!(round.df("k8s"), 5);
        // マルチバイト (fst の Levenshtein が取りこぼしたケース)
        let v = Vocab::from_unsorted([("検索".into(), 9), ("検討".into(), 7), ("ロールバック".into(), 3)]);
        let c: Vec<_> = v.candidates("検策", 1).into_iter().map(|c| c.term).collect();
        assert_eq!(c, ["検索", "検討"]);
        assert_eq!(v.candidates("ロールバク", 1)[0].term, "ロールバック");
    }
}
