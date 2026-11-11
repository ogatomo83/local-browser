//! 検索の中心部分 (lb-core)。
//!
//! ファイル操作・ネットワーク・DB・スレッドを使わない。ネイティブ (lb) と
//! ブラウザ用 wasm (wasm32-unknown-unknown) の両方で同じコードを動かすため。
//! CI / `make check-wasm` で wasm のビルドが通ることを毎回確かめる。

pub mod doc;
pub mod hash;
pub mod highlight;
pub mod index;
pub mod ingest;
pub mod manifest;
pub mod normalize;
pub mod query;
pub mod readings;
pub mod search;
pub mod segment;
pub mod tokenize;
pub mod vocab;

pub use normalize::{NORM_VERSION, normalize};
pub use query::{Fix, Plan, TermPlan, plan};
pub use vocab::Vocab;
