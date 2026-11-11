// 黄金のクエリ集を、ブラウザ用 wasm (web/dist/lb.wasm) と Worker と同じ lb.js で回す。
// lb eval と同じ判定。ブラウザがなくても、モード C の検索が lb と同じ答えを返すかを確かめられる。
//   node scripts/web-eval.mjs golden/docs-test.json docs/test fixtures/confluence-dev.json
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative, basename } from "node:path";
import { Lb } from "../web/www/lb.js";

const [golden, dir, fixture] = process.argv.slice(2);
const lb = await Lb.create(readFileSync("web/dist/lb.wasm"));
lb.call({ op: "open", build: lb.build });
const exts = lb.call({ op: "status" }).extensions;
const SKIP = new Set(["node_modules", "target", "dist", "build", "vendor", "__pycache__"]);

// フォルダ (Worker では FileSystemDirectoryHandle を歩くところ)
const root = basename(dir);
const files = [];
(function walk(d) {
  for (const name of readdirSync(d)) {
    if (name.startsWith(".")) continue;
    const p = join(d, name);
    const st = statSync(p);
    if (st.isDirectory()) { if (!SKIP.has(name)) walk(p); continue; }
    if (!exts.includes(name.split(".").pop().toLowerCase())) continue;
    const rel = relative(dir, p);
    files.push({ key: `${root}/${rel}`, name: rel, version: `${st.mtimeMs}-${st.size}`, mtime: Math.floor(st.mtimeMs / 1000), bytes: () => readFileSync(p) });
  }
})(dir);
// Confluence の模擬データ (lb の source.rs と同じ形にする)
if (fixture) {
  const escape = s => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  for (const s of JSON.parse(readFileSync(fixture, "utf8")).spaces) {
    for (const p of s.pages) {
      const html = `<html><head><meta charset="utf-8"><title>${escape(p.title)}</title></head><body>${p.body}</body></html>`;
      files.push({ key: `confluence:${s.key}/${p.id}`, rel: `${s.key} / ${p.title}`, name: `${s.key}-${p.id}.html`, version: `v${p.version}`, mtime: p.updated_at, scope: p.restricted_to || "public", bytes: () => Buffer.from(html) });
    }
  }
}

const t0 = performance.now();
const { read } = lb.call({ op: "scan", items: files.map(({ key, version, mtime, scope, rel }) => ({ key, version, mtime, scope, rel })) });
const byKey = new Map(files.map(f => [f.key, f]));
for (const key of read) {
  const f = byKey.get(key);
  const r = lb.call({ op: "put", key, version: f.version, mtime: f.mtime, scope: f.scope, name: f.name }, f.bytes());
  if (r.status === "failed") console.log("  ! 読み取れません", key, r.error);
}
const c = lb.call({ op: "commit" });
const st = lb.call({ op: "status" });
console.log(`engine: lb-web (wasm ${(readFileSync("web/dist/lb.wasm").length / 1e6).toFixed(2)}MB) / 索引: ${st.docs} 文書, セグメント ${st.segments}, 語の一覧: ${st.vocab} 語 (${Math.round(performance.now() - t0)}ms)\n`);

const cases = JSON.parse(readFileSync(golden, "utf8"));
let fails = 0, pend = [0, 0];
const times = [];
const pad = (s, n) => s + " ".repeat(Math.max(0, n - [...s].reduce((w, ch) => w + (ch.charCodeAt(0) > 0xff ? 2 : 1), 0)));
for (const cs of cases) {
  const k = cs.k ?? 3;
  const t = performance.now();
  const o = lb.call({ op: "search", q: cs.q, limit: 10, scopes: cs.scopes ?? null });
  times.push(performance.now() - t);
  const why = [];
  if (cs.mode && cs.mode !== o.mode) why.push(`段階 ${o.mode} (期待 ${cs.mode})`);
  const top = o.hits.slice(0, k);
  if (cs.top?.length && !top.some(h => cs.top.some(x => h.rel.includes(x)))) why.push(`上位 ${k} 件に ${JSON.stringify(cs.top)} がない`);
  const bad = top.find(h => (cs.not_top || []).some(x => h.rel.includes(x)));
  if (bad) why.push(`上位 ${k} 件に出てはいけない ${bad.rel} が出た`);
  if (cs.corrected != null && o.corrected !== cs.corrected) why.push(`補正 ${o.corrected} (期待 ${cs.corrected})`);
  if (cs.suggest != null && o.suggestion !== cs.suggest) why.push(`もしかして ${o.suggestion} (期待 ${cs.suggest})`);
  const line = `${pad(cs.q, 22)} ${pad(o.mode, 9)} ${o.total}件${o.corrected ? " → " + o.corrected : ""}`;
  if (cs.pending) { pend[1]++; if (!why.length) pend[0]++; console.log(`  ${why.length ? "--" : "ok"}  ${line} (保留)`); continue; }
  if (why.length) { fails++; console.log(`  NG  ${line} ${JSON.stringify(o.hits.slice(0, 3).map(h => h.rel))}`); why.forEach(w => console.log(`        - ${w}`)); }
  else console.log(`  ok  ${line}`);
}
times.sort((a, b) => a - b);
const n = cases.length - pend[1];
console.log(`\n検索時間: 中央値 ${times[times.length >> 1].toFixed(2)}ms / 最大 ${times.at(-1).toFixed(2)}ms`);
console.log(`\n合計 ${n - fails}/${n}  (保留 ${pend[1]} 本のうち ${pend[0]} 本は今でも通る)`);
process.exit(fails ? 1 : 0);
