// モード C を本物の Chrome (headless) で通しで動かす。CDP で直接操作する (Chrome が要るので CI には入れていない)。
//   make web-e2e      (CHROME=... で Chrome の場所を変えられる。スクリーンショットは target/web-e2e/)
// ネイティブのフォルダ選択の窓は自動では押せないので、<input webkitdirectory> と同じ形 ({path, file}) で
// docs/test を渡す。取り込み → 検索 → 文書 → 再読み込み (IndexedDB から復元) → 選び直し (差分) を確かめる。
import { spawn } from "node:child_process";
import { mkdtempSync, writeFileSync, mkdirSync, readdirSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, relative } from "node:path";

const OUT = "target/web-e2e";
mkdirSync(OUT, { recursive: true });
const BASE = "http://127.0.0.1:8091";
const URL0 = `${BASE}/web/dist/index.html`;
const FILES = [];
(function walk(d) { for (const n of readdirSync(d)) { const p = join(d, n); if (statSync(p).isDirectory()) walk(p); else if (/\.(md|txt|html)$/.test(n)) FILES.push(relative("docs", p)); } })("docs/test");
const server = spawn("python3", ["-m", "http.server", "8091", "--bind", "127.0.0.1", "--directory", "."], { stdio: "ignore" });
const prof = mkdtempSync(join(tmpdir(), "lb-chrome-"));
const chrome = spawn(process.env.CHROME || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  ["--headless=new", "--remote-debugging-port=9333", `--user-data-dir=${prof}`, "--no-first-run", "--window-size=1280,900", "about:blank"], { stdio: "ignore" });
const sleep = ms => new Promise(r => setTimeout(r, ms));
let targets;
for (let i = 0; i < 50; i++) { try { targets = await (await fetch("http://127.0.0.1:9333/json")).json(); break; } catch { await sleep(200); } }
const page = targets.find(t => t.type === "page");
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise(r => ws.onopen = r);
let id = 0; const pend = new Map(); const logs = [];
ws.onmessage = e => {
  const m = JSON.parse(e.data);
  if (m.id) { pend.get(m.id)?.(m); pend.delete(m.id); return; }
  if (m.method === "Runtime.consoleAPICalled") logs.push(`[${m.params.type}] ` + m.params.args.map(a => a.value ?? a.description).join(" "));
  if (m.method === "Runtime.exceptionThrown") logs.push("[exception] " + JSON.stringify(m.params.exceptionDetails.exception?.description || m.params.exceptionDetails.text));
  if (m.method === "Log.entryAdded") logs.push(`[log ${m.params.entry.level}] ${m.params.entry.text}`);
};
const send = (method, params = {}) => new Promise(r => { const i = ++id; pend.set(i, r); ws.send(JSON.stringify({ id: i, method, params })); });
const ev = async expr => {
  const r = await send("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true });
  if (r.result.exceptionDetails) throw new Error(JSON.stringify(r.result.exceptionDetails.exception?.description || r.result.exceptionDetails));
  return r.result.result.value;
};
const shot = async name => { const r = await send("Page.captureScreenshot", { format: "png" }); writeFileSync(join(OUT, name), Buffer.from(r.result.data, "base64")); };
const waitIdle = async () => { for (let i = 0; i < 100; i++) { const s = await ev(`lbBackend.api("/api/status")`); if (!s.indexing && s.last) return s; await sleep(150); } throw new Error("timeout"); };
let fails = 0;
const check = (name, ok, extra = "") => (ok || fails++, console.log(`${ok ? "ok" : "NG"}  ${name}${extra ? "  " + extra : ""}`));

await send("Runtime.enable"); await send("Log.enable"); await send("Page.enable");
await send("Page.navigate", { url: URL0 }); await sleep(1500);
let s = await ev(`lbBackend.api("/api/status")`);
check("起動直後は空", s.docs === 0 && s.roots.length === 0, JSON.stringify({ docs: s.docs }));
await shot("1-empty.png");

// webkitdirectory で選んだのと同じ形 ({path, file}) を作って渡す
const addFiles = (mutate = "") => ev(`(async () => {
  const files = [];
  for (const p of ${JSON.stringify(FILES)}) {
    let text = await (await fetch("/docs/" + p.split("/").map(encodeURIComponent).join("/"))).text();
    ${mutate}
    if (text == null) continue;
    files.push({ path: p, file: new File([text], p.split("/").pop(), { lastModified: p.includes("deploy") && ${JSON.stringify(!!mutate)} ? 2000000000000 : 1700000000000 }) });
  }
  return lbBackend.addFiles(files);
})()`);
await addFiles();
s = await waitIdle();
check("取り込み", s.docs === 16 && s.last.added === 16, JSON.stringify(s.last));
let r = await ev(`lbBackend.api("/api/search?q=" + encodeURIComponent("デプロイ手順"))`);
check("検索 デプロイ手順", r.mode === "exact" && r.hits[0].rel === "runbooks/deploy.md", `${r.total}件 ${r.hits[0]?.rel}`);
r = await ev(`lbBackend.api("/api/search?q=kubernets")`);
check("打ち間違いの補正 kubernets", r.mode === "corrected" && r.corrected === "Kubernetes", r.corrected);
r = await ev(`lbBackend.api("/api/search?q=secret")`);
check("隠しフォルダ・node_modules は入らない", !r.hits.some(h => /hidden|node_modules/.test(h.rel)), r.hits.map(h => h.rel).join(","));

// 画面: 検索結果 → 文書
await send("Page.navigate", { url: `${URL0}?q=${encodeURIComponent("ロールバック")}` }); await sleep(1200);
const resultsText = await ev(`document.querySelector("#view").innerText`);
check("検索結果の画面", /runbooks/.test(resultsText) && /件/.test(resultsText), resultsText.split("\n").slice(0, 3).join(" / "));
await shot("2-results.png");
await ev(`document.querySelector("a.title").click()`); await sleep(800);
const docText = await ev(`document.querySelector("#view").innerText`);
const marks = await ev(`document.querySelectorAll("mark.hl").length`);
check("文書を開く (強調あり)", /ロールバック/.test(docText) && marks > 0, `mark ${marks} / ${await ev("location.search")}`);
check("URL は ?doc=", (await ev("location.search")).startsWith("?doc="));
await shot("3-doc.png");
// XSS の文書: スクリプトが動かない
await send("Page.navigate", { url: `${URL0}?doc=${encodeURIComponent("test/edge-cases/xss.md")}` }); await sleep(1000);
check("xss.md でスクリプトが動かない", (await ev("window.__xss === undefined && !document.querySelector('#view script')")) === true);
// HTML 文書は sandbox の iframe
await send("Page.navigate", { url: `${URL0}?doc=${encodeURIComponent("test/runbooks/incident.html")}` }); await sleep(1000);
check("HTML 文書は sandbox の枠", (await ev(`document.querySelector("iframe.htmlframe")?.getAttribute("sandbox")`)) === "");

// リロード: 索引は IndexedDB から。フォルダ (files) は選び直していないので、前回のまま残る
await send("Page.navigate", { url: URL0 }); await sleep(1500);
s = await waitIdle();
check("再読み込み後も索引が残る", s.docs === 16 && s.last.unchanged === 16 && s.last.added === 0, JSON.stringify(s.last));
check("フォルダは「選び直すと最新」", s.rootsInfo[0]?.permission === "files", JSON.stringify(s.rootsInfo));
r = await ev(`lbBackend.api("/api/search?q=" + encodeURIComponent("障害対応"))`);
check("再読み込み後の検索", r.total > 0, `${r.total}件`);
await shot("4-home-after-reload.png");

// 選び直す: deploy を書き換え、faq.txt を消した
await addFiles(`if (p.endsWith("faq.txt")) text = null; if (p.endsWith("deploy.md")) text += "\\n\\n## 新しい節\\n\\nカナリアリリースで段階的に出す。";`);
s = await waitIdle();
check("差分: 変更 1 / 削除 1", s.last.updated === 1 && s.last.deleted === 1 && s.last.unchanged === 14, JSON.stringify(s.last));
r = await ev(`lbBackend.api("/api/search?q=" + encodeURIComponent("カナリアリリース"))`);
check("変更が検索に出る", r.hits[0]?.rel === "runbooks/deploy.md", `${r.total}件`);
r = await ev(`lbBackend.api("/api/search?q=" + encodeURIComponent("経費精算"))`);
check("消したものは出ない", !r.hits.some(h => h.rel === "faq.txt"), `${r.total}件`);
const objs = await ev(`new Promise(res => { const q = indexedDB.open("local-browser"); q.onsuccess = () => { const t = q.result.transaction("objects").objectStore("objects").getAllKeys(); t.onsuccess = () => res(t.result.length); }; })`);
check("使われなくなった objects は消えている", objs === 15 + (s.last.segments ?? 0), `objects ${objs}, segments ${s.last.segments}`);

const bad = logs.filter(l => /error|exception|Content Security|Refused/i.test(l));
check("コンソールにエラー / CSP 違反なし", bad.length === 0, bad.slice(0, 5).join(" | "));
writeFileSync(join(OUT, "console.log"), logs.join("\n"));
ws.close(); chrome.kill(); server.kill();
process.exit(fails ? 1 : 0);
