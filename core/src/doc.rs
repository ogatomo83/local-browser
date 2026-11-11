//! 抽出結果 (タイトル・見出し・本文・表示用 HTML)。
//!
//! JSON にして、その BLAKE3 を名前に置く。lb とブラウザ (モード C) で同じ形・同じ並びにして、
//! 同じ文書なら同じ名前になるようにする (モード B で静的に配るときに、そのまま使える)。

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct Extracted {
    pub title: String,
    pub headings: Vec<HeadingOut>,
    pub body: String,
    pub html: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct HeadingOut {
    pub level: u8,
    pub text: String,
    pub anchor: String,
}

impl Extracted {
    pub fn headings_text(&self) -> String {
        self.headings.iter().map(|h| h.text.as_str()).collect::<Vec<_>>().join("\n")
    }

    /// 置くときのバイト列と、その名前
    pub fn to_blob(&self) -> (String, Vec<u8>) {
        let bytes = serde_json::to_vec(self).expect("文字列だけなので失敗しない");
        (crate::hash::content_id(&bytes), bytes)
    }

    pub fn from_blob(bytes: &[u8]) -> Result<Self, String> {
        serde_json::from_slice(bytes).map_err(|e| e.to_string())
    }
}
