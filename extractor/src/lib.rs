//! 文書の中身を読み取る wasm コンポーネント。
//! wasm32-wasip2 向けにビルドし、ホスト (wasmtime) から呼ばれる。
//! ファイルシステム・ネットワークへのアクセス権は与えられない。
//! 中身は lb-extract (ブラウザ用の wasm でも同じものを使う)。

wit_bindgen::generate!({
    world: "extractor",
    path: "../wit",
});

use exports::local_browser::extractor::extract::{Document, Guest, Heading};

struct Extractor;

impl Guest for Extractor {
    fn supported_extensions() -> Vec<String> {
        lb_extract::EXTENSIONS.iter().map(|s| s.to_string()).collect()
    }

    fn extract(path: String, content: Vec<u8>) -> Result<Document, String> {
        let d = lb_extract::extract(&path, &content)?;
        Ok(Document {
            title: d.title,
            headings: d
                .headings
                .into_iter()
                .map(|h| Heading { level: h.level, text: h.text, anchor: h.anchor })
                .collect(),
            body: d.body,
            html: d.html,
        })
    }
}

export!(Extractor);
