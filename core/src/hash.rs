//! 内容アドレスの名前。中身の BLAKE3 を "b3-" + 16 進で表す。

pub fn content_id(bytes: &[u8]) -> String {
    format!("b3-{}", blake3::hash(bytes).to_hex())
}

/// いくつかの要素をつないだもののハッシュ (キャッシュのキー用)。区切りの曖昧さを避けるため長さを前に付ける
pub fn key_id(parts: &[&[u8]]) -> String {
    let mut h = blake3::Hasher::new();
    for p in parts {
        h.update(&(p.len() as u64).to_le_bytes());
        h.update(p);
    }
    format!("b3-{}", h.finalize().to_hex())
}
