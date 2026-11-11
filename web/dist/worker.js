// モード C の Worker。画面 (mode-c.js) から頼まれて、取り込みと検索をする。
//
// 起動したら、IndexedDB に置いてある索引をすぐ開いて検索できるようにし、それから
// 選んであるフォルダを歩いて、変わったファイルだけ読み直す (リロード = 差分の取り込み)。
// ファイルは読むだけ。書き込みも、どこかへの送信もしない。

import { Lb } from "./lb.js";
import * as store from "./store.js";
import { walkHandle, fromFiles, resolvePath, fileInHandle } from "./scan.js";

const enc = new TextEncoder();
const dec = new TextDecoder();
const PER = 20;

let lb;
let sources = {};       // manifest.sources (消えたフォルダ以外の前回の状態を残すのに使う)
let generation = -1;
let status = {};
let indexing = false;
let progress = null;
let last = null;
let rootsCache = [];
const transient = new Map(); // フォルダ名 → webkitdirectory で選んだファイル (このページを開いている間だけ)
const bc = typeof BroadcastChannel === "function" ? new BroadcastChannel("local-browser") : null;

async function init() {
  const wasm = await (await fetch("lb.wasm")).arrayBuffer();
  lb = await Lb.create(wasm);
  await load();
  reload();
}

/** IndexedDB に置いてある索引を開く */
async function load() {
  const saved = (await store.get("kv", "manifest")) || "";
  let o = lb.call({ op: "open", build: lb.build }, enc.encode(saved));
  if (o.reset) {
    await store.clearIndex();
    o = lb.call({ op: "open", build: lb.build });
  }
  for (const id of o.segments) {
    const bytes = await store.get("objects", id);
    if (!bytes) {
      // 置き場が壊れている。作り直す
      console.warn("[lb] セグメントがありません。作り直します", id);
      await store.clearIndex();
      lb.call({ op: "open", build: lb.build });
      break;
    }
    lb.call({ op: "segment", id }, bytes);
  }
  const { ids } = lb.call({ op: "blobs" });
  for (const id of ids) {
    const bytes = await store.get("objects", id);
    if (bytes) lb.call({ op: "blob", id }, bytes);
  }
  status = lb.call({ op: "ready" });
  generation = status.generation;
  sources = saved && !o.reset ? JSON.parse(saved).sources || {} : {};
  rootsCache = (await store.get("kv", "roots")) || [];
}

async function roots() {
  rootsCache = (await store.get("kv", "roots")) || [];
  return rootsCache;
}

async function permission(r) {
  if (r.kind === "files") return transient.has(r.name) ? "granted" : "files";
  try { return await r.handle.queryPermission({ mode: "read" }); } catch { return "denied"; }
}

/** リロード = 選んだフォルダを歩いて、変わったものだけ読み直す */
function reload() {
  if (indexing) return false;
  indexing = true;
  emit();
  const run = async () => {
    const started = performance.now();
    try {
      // 別のタブが先に書き換えていたら開き直す
      const saved = await store.get("kv", "manifest");
      const g = saved ? JSON.parse(saved).generation : 0;
      if (g !== generation) await load();

      const exts = status.extensions;
      const items = [];
      for (const r of await roots()) {
        const perm = await permission(r);
        let got = null;
        if (perm === "granted") {
          progress = { phase: "scan", root: r.name };
          emit();
          try {
            got = r.kind === "files" ? fromFiles(r.name, transient.get(r.name), exts) : await walkHandle(r.handle, r.name, exts);
          } catch (e) {
            console.warn("[lb] フォルダを読めません", r.name, e);
          }
        }
        if (got) items.push(...got);
        else {
          // 読めないフォルダ (許可がない / 選び直していない) は、前回のまま残す (消えた扱いにしない)
          for (const [key, s] of Object.entries(sources)) if (key.startsWith(r.name + "/")) items.push({ key, version: s.version, mtime: s.mtime });
        }
      }
      const byKey = new Map(items.map(it => [it.key, it]));
      const { read } = lb.call({ op: "scan", items: items.map(({ key, version, mtime }) => ({ key, version, mtime })) });
      let i = 0;
      for (const key of read) {
        const it = byKey.get(key);
        if (i++ % 10 === 0) { progress = { phase: "read", done: i - 1, total: read.length }; emit(); }
        try {
          const bytes = new Uint8Array(await (await it.open()).arrayBuffer());
          const r = lb.call({ op: "put", key, version: it.version, mtime: it.mtime, name: it.name }, bytes);
          if (r.status === "failed") console.warn("[lb] 読み取れません", key, r.error);
        } catch (e) {
          console.warn("[lb] 読めません", key, e);
          lb.call({ op: "failed", key });
        }
      }
      const c = lb.call({ op: "commit" });
      const objects = c.put.map(id => [id, lb.call({ op: "take", id })]);
      if (c.save || objects.length) {
        await store.commit({ objects, manifest: JSON.stringify(c.manifest), keep: c.changed ? new Set(c.keep) : null });
      }
      sources = c.manifest.sources;
      generation = c.generation;
      status = lb.call({ op: "status" });
      last = { ...c.counts, segments: c.segments, merged: c.merged, generation: c.generation, millis: Math.round(performance.now() - started), finished_at: Math.floor(Date.now() / 1000) };
      if (c.save) bc?.postMessage({ generation });
    } catch (e) {
      console.error("[lb] 取り込みに失敗しました", e);
      last = { error: String(e?.message || e), finished_at: Math.floor(Date.now() / 1000) };
    } finally {
      indexing = false;
      progress = null;
      emit();
    }
  };
  // 同じフォルダを開いている別のタブと同時に書かないように
  if (navigator.locks) navigator.locks.request("local-browser-ingest", run);
  else run();
  return true;
}

bc && (bc.onmessage = async e => {
  if (!indexing && e.data?.generation !== generation) { await load(); emit(); }
});

async function statusInfo() {
  const rs = await Promise.all(rootsCache.map(async r => ({ name: r.name, kind: r.kind, permission: await permission(r) })));
  return {
    mode: "c",
    engine: "segments",
    docs: status.docs || 0,
    vocab: status.vocab || 0,
    extensions: status.extensions || [],
    roots: rootsCache.map(r => r.name),
    rootsInfo: rs,
    indexing,
    progress,
    last,
  };
}

function emit() {
  statusInfo().then(s => postMessage({ event: "status", value: s }));
}

function uniqueName(name, list) {
  let n = name, i = 2;
  while (list.some(r => r.name === n)) n = `${name}~${i++}`;
  return n;
}

async function docOf(id) {
  const meta = lb.call({ op: "doc", id });
  let bytes = await store.get("objects", meta.blob);
  if (!bytes) { await load(); bytes = await store.get("objects", lb.call({ op: "doc", id }).blob); }
  if (!bytes) throw Object.assign(new Error("not found"), { status: 404 });
  const d = JSON.parse(dec.decode(bytes));
  return { ...meta, path: id, headings: d.headings, html: d.html };
}

const IMG = { png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", gif: "image/gif", webp: "image/webp", svg: "image/svg+xml", avif: "image/avif" };

/** 文書 from からの相対パスにあるファイル。読めるのは選んだフォルダの中だけ */
async function fileFrom(from, href) {
  const meta = lb.call({ op: "doc", id: from });
  const rel = resolvePath(meta.rel, href);
  if (rel == null) throw Object.assign(new Error("outside"), { status: 403 });
  const r = rootsCache.find(x => x.name === meta.root);
  if (!r) throw Object.assign(new Error("not found"), { status: 404 });
  let file;
  if (r.kind === "files") file = transient.get(r.name)?.find(f => f.path === `${r.name}/${rel}`)?.file;
  else if ((await permission(r)) === "granted") file = await fileInHandle(r.handle, rel).catch(() => null);
  if (!file) throw Object.assign(new Error("not found"), { status: 404 });
  return { key: `${meta.root}/${rel}`, file };
}

const ops = {
  status: () => statusInfo(),
  async search({ q, page = 0, exact = false }) {
    const t = performance.now();
    const r = lb.call({ op: "search", q, exact, limit: PER, offset: page * PER });
    return { ...r, q, page, per: PER, engine: "segments", millis: performance.now() - t };
  },
  suggest: ({ q }) => lb.call({ op: "search", q, limit: 8 }).hits.map(h => ({ id: h.id, title: h.title, rel: h.rel })),
  recent: () => lb.call({ op: "recent", limit: 12 }),
  doc: ({ id }) => docOf(id),
  async resolve({ from, href }) {
    const meta = lb.call({ op: "doc", id: from });
    const rel = resolvePath(meta.rel, href);
    if (rel == null) throw Object.assign(new Error("outside"), { status: 403 });
    const key = `${meta.root}/${rel}`;
    try { lb.call({ op: "doc", id: key }); return { id: key }; } catch {}
    const { file } = await fileFrom(from, href);
    // 文書でないファイルは、ページの中で動かないよう画像と PDF 以外は文字として開く
    const ext = rel.split(".").pop().toLowerCase();
    const type = ext === "pdf" ? "application/pdf" : ext !== "svg" && IMG[ext] ? IMG[ext] : "text/plain;charset=utf-8";
    return { blob: new Blob([file], { type }) };
  },
  async image({ from, href }) {
    const { file } = await fileFrom(from, href);
    const ext = href.split(/[?#]/)[0].split(".").pop().toLowerCase();
    if (!IMG[ext]) throw Object.assign(new Error("not an image"), { status: 415 });
    return new Blob([file], { type: IMG[ext] });
  },
  reindex: () => ({ started: reload() }),
  handles: async () => (await roots()).filter(r => r.kind === "handle").map(r => ({ name: r.name, handle: r.handle })),
  async addRoot({ handle }) {
    const list = await roots();
    for (const r of list) if (r.kind === "handle" && (await r.handle.isSameEntry(handle).catch(() => false))) { reload(); return { name: r.name, existed: true }; }
    const name = uniqueName(handle.name, list);
    list.push({ name, kind: "handle", handle });
    await store.set("kv", "roots", list);
    rootsCache = list;
    reload();
    return { name };
  },
  async addFiles({ name, files }) {
    const list = await roots();
    if (!list.some(r => r.name === name && r.kind === "files")) {
      name = uniqueName(name, list);
      list.push({ name, kind: "files" });
      await store.set("kv", "roots", list);
    }
    rootsCache = list;
    transient.set(name, files);
    reload();
    return { name };
  },
  async removeRoot({ name }) {
    const list = (await roots()).filter(r => r.name !== name);
    await store.set("kv", "roots", list);
    rootsCache = list;
    transient.delete(name);
    // 一覧から外したので、次のリロードでそのフォルダの文書は消えた扱いになる
    reload();
    return {};
  },
};

const ready = init().catch(e => { console.error("[lb] 起動に失敗しました", e); throw e; });

onmessage = async e => {
  const { id, op, args } = e.data;
  try {
    await ready;
    if (!ops[op]) throw new Error(`unknown op ${op}`);
    postMessage({ id, ok: true, value: await ops[op](args || {}) });
  } catch (err) {
    postMessage({ id, ok: false, error: String(err?.message || err), status: err?.status || 500 });
  }
};
