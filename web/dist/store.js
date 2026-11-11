// IndexedDB の置き場。lb の `<db>.store/` と同じ形:
//   kv/manifest   いまの索引の目次 (書き換えるのはこれだけ)
//   kv/roots      選んだフォルダ ({name, kind: "handle" | "files", handle?})
//   objects/<id>  セグメントと抽出結果。名前は中身の BLAKE3、書いたら変えない
// 取り込みの結果は 1 つのトランザクションで書く (置く → manifest → 使われなくなったものを消す)。
// 途中でタブを閉じても、前の状態か新しい状態のどちらかになる。

const DB = "local-browser";
let dbp;

export function open() {
  dbp ??= new Promise((resolve, reject) => {
    const r = indexedDB.open(DB, 1);
    r.onupgradeneeded = () => { r.result.createObjectStore("kv"); r.result.createObjectStore("objects"); };
    r.onsuccess = () => resolve(r.result);
    r.onerror = () => reject(r.error);
  });
  return dbp;
}

const req = r => new Promise((resolve, reject) => { r.onsuccess = () => resolve(r.result); r.onerror = () => reject(r.error); });

export async function get(store, key) {
  const db = await open();
  return req(db.transaction(store).objectStore(store).get(key));
}

export async function set(store, key, value) {
  const db = await open();
  const tx = db.transaction(store, "readwrite");
  tx.objectStore(store).put(value, key);
  return done(tx);
}

function done(tx) {
  return new Promise((resolve, reject) => { tx.oncomplete = () => resolve(); tx.onerror = tx.onabort = () => reject(tx.error); });
}

/** 取り込みの結果を書く。objects: [[id, bytes]], keep: 残す名前の集合 (null なら消さない) */
export async function commit({ objects, manifest, keep }) {
  const db = await open();
  const tx = db.transaction(["objects", "kv"], "readwrite");
  const os = tx.objectStore("objects");
  for (const [id, bytes] of objects) os.put(bytes, id);
  if (manifest) tx.objectStore("kv").put(manifest, "manifest");
  let removed = 0;
  if (keep) {
    const keys = await req(os.getAllKeys());
    for (const k of keys) if (!keep.has(k)) { os.delete(k); removed++; }
  }
  await done(tx);
  return removed;
}

/** すべて消す (wasm の版が変わって作り直すとき)。選んだフォルダは残す */
export async function clearIndex() {
  const db = await open();
  const tx = db.transaction(["objects", "kv"], "readwrite");
  tx.objectStore("objects").clear();
  tx.objectStore("kv").delete("manifest");
  return done(tx);
}
