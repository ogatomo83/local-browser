// 生成物: host/ui/index.html のスクリプト (scripts/build-web.mjs)
"use strict";
// ======================================================================
// 内部 URL:  lb://newtab   lb://search?q=..&page=N&exact=1   lb://doc/ID?hl=..#anchor
// exact=1 は補正しない検索 (「元の語のまま検索」)
// 実際の location とは /, /search?q=.., /doc/ID?hl=..#anchor で対応させる
// ======================================================================

const $ = (s, el = document) => el.querySelector(s);
const esc = s => String(s ?? "").replace(/[&<>"']/g, c => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
const view = $("#view");
const omni = $("#omni");

const ICON = {
  search: '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round"><circle cx="11" cy="11" r="7"/><path d="M20 20l-3.5-3.5"/></svg>',
  doc: '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round"><path d="M14 3H6a1 1 0 0 0-1 1v16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1V8z"/><path d="M14 3v5h5"/></svg>',
  home: '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round"><path d="M3 11l9-7 9 7v9a1 1 0 0 1-1 1h-5v-6h-6v6H4a1 1 0 0 1-1-1z"/></svg>',
  x: '<svg width="10" height="10" viewBox="0 0 10 10" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"><path d="M1 1l8 8M9 1l-8 8"/></svg>',
  plus: '<svg width="16" height="16" viewBox="0 0 16 16" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M8 2v12M2 8h12"/></svg>',
};

// ---------------------------------------------------------------- URL
function parse(url) {
  const m = /^lb:\/\/([a-z]+)(\/[^?#]*)?(\?[^#]*)?(#.*)?$/.exec(url || "");
  if (!m) return { kind: "newtab" };
  const params = new URLSearchParams(m[3] || "");
  const hash = m[4] ? decodeURIComponent(m[4].slice(1)) : "";
  if (m[1] === "search") return { kind: "search", q: params.get("q") || "", page: +params.get("page") || 0, exact: params.get("exact") === "1" };
  if (m[1] === "doc") return { kind: "doc", id: decodeURIComponent((m[2] || "/").slice(1)), hl: params.get("hl") || "", hash };
  return { kind: "newtab" };
}
const searchUrl = (q, page = 0, exact = false) => `lb://search?q=${encodeURIComponent(q)}${page ? "&page=" + page : ""}${exact ? "&exact=1" : ""}`;
const docUrl = (id, hl = "", hash = "") => `lb://doc/${encodeURIComponent(id)}${hl ? "?hl=" + encodeURIComponent(hl) : ""}${hash ? "#" + encodeURIComponent(hash) : ""}`;
// モード C (静的ページ) はどのパスでも同じ index.html なので、?q= / ?doc= で表す
const BACKEND = window.lbBackend;
function toLocation(url) {
  const p = parse(url);
  if (BACKEND) {
    const base = location.pathname;
    if (p.kind === "search") return `${base}?q=${encodeURIComponent(p.q)}${p.page ? "&page=" + p.page : ""}${p.exact ? "&exact=1" : ""}`;
    if (p.kind === "doc") return `${base}?doc=${encodeURIComponent(p.id)}${p.hl ? "&hl=" + encodeURIComponent(p.hl) : ""}${p.hash ? "#" + encodeURIComponent(p.hash) : ""}`;
    return base;
  }
  if (p.kind === "search") return `/search?q=${encodeURIComponent(p.q)}${p.page ? "&page=" + p.page : ""}${p.exact ? "&exact=1" : ""}`;
  if (p.kind === "doc") return `/doc/${encodeURIComponent(p.id)}${p.hl ? "?hl=" + encodeURIComponent(p.hl) : ""}${p.hash ? "#" + encodeURIComponent(p.hash) : ""}`;
  return "/";
}
function fromLocation() {
  const u = new URL(location.href);
  if (BACKEND) {
    if (u.searchParams.get("q")) return searchUrl(u.searchParams.get("q"), +u.searchParams.get("page") || 0, u.searchParams.get("exact") === "1");
    if (u.searchParams.get("doc")) return docUrl(u.searchParams.get("doc"), u.searchParams.get("hl") || "", decodeURIComponent(u.hash.slice(1)));
    return null;
  }
  if (u.pathname === "/search" && u.searchParams.get("q")) return searchUrl(u.searchParams.get("q"), +u.searchParams.get("page") || 0, u.searchParams.get("exact") === "1");
  const m = /^\/doc\/([^/]+)$/.exec(u.pathname);
  if (m) return docUrl(decodeURIComponent(m[1]), u.searchParams.get("hl") || "", decodeURIComponent(u.hash.slice(1)));
  return null;
}

// ---------------------------------------------------------------- API
// モード C では Worker (mode-c.js) が答える
async function api(path, opts = {}) {
  if (BACKEND) return BACKEND.api(path, opts);
  const r = await fetch(path, { ...opts, headers: { "x-lb": "1", ...(opts.headers || {}) } });
  if (!r.ok) throw Object.assign(new Error(await r.text()), { status: r.status });
  return r.json();
}

// ---------------------------------------------------------------- tabs
let tabs = [];
let active = 0;
let seq = 1;
const store = {
  load() { try { return JSON.parse(localStorage.getItem("lb.tabs") || "null"); } catch { return null; } },
  save() { try { localStorage.setItem("lb.tabs", JSON.stringify({ tabs: tabs.map(t => ({ stack: t.stack, i: t.i, title: t.title })), active })); } catch {} },
};
const cur = () => tabs[active];
function newTab(url = "lb://newtab", { activate = true, after = true } = {}) {
  const t = { id: seq++, stack: [url], i: 0, title: "新しいタブ", scroll: {} };
  const at = after ? active + 1 : tabs.length;
  tabs.splice(Math.min(at, tabs.length), 0, t);
  if (activate) { active = tabs.indexOf(t); render(); }
  else { renderTabs(); store.save(); }
  return t;
}
function closeTab(idx) {
  tabs.splice(idx, 1);
  if (!tabs.length) { tabs.push({ id: seq++, stack: ["lb://newtab"], i: 0, title: "新しいタブ", scroll: {} }); active = 0; }
  else if (idx < active || active >= tabs.length) active = Math.max(0, active - 1);
  render();
}
function switchTab(idx) {
  if (idx < 0 || idx >= tabs.length || idx === active) return;
  saveScroll();
  active = idx;
  render();
}
function navigate(url, { newTabBg = false } = {}) {
  if (newTabBg) { newTab(url, { activate: false }); toast("新しいタブで開きました"); return; }
  saveScroll();
  const t = cur();
  t.stack = t.stack.slice(0, t.i + 1);
  if (t.stack[t.i] !== url) { t.stack.push(url); t.i++; }
  render();
}
function go(delta) {
  const t = cur();
  const n = t.i + delta;
  if (n < 0 || n >= t.stack.length) return;
  saveScroll();
  t.i = n;
  render({ restoreScroll: true });
}
function saveScroll() { const t = cur(); if (t) t.scroll[t.i] = view.scrollTop; }

function renderTabs() {
  const el = $("#tabs");
  el.innerHTML = tabs.map((t, i) => {
    const kind = parse(t.stack[t.i]).kind;
    const ic = kind === "doc" ? ICON.doc : kind === "search" ? ICON.search : ICON.home;
    return `<div class="tab${i === active ? " active" : ""}" data-i="${i}" title="${esc(t.title)}">
      <span class="ico">${ic}</span><span class="ttl">${esc(t.title)}</span>
      <button class="x" data-close="${i}" title="タブを閉じる (Alt+W)">${ICON.x}</button></div>`;
  }).join("") + `<button class="newtab-btn" id="plus" title="新しいタブ (Alt+T)">${ICON.plus}</button>`;
}
$("#tabs").addEventListener("mousedown", e => {
  const x = e.target.closest("[data-close]");
  if (x) { e.preventDefault(); closeTab(+x.dataset.close); return; }
  if (e.target.closest("#plus")) { newTab(); setTimeout(() => focusSearch(), 0); return; }
  const t = e.target.closest(".tab");
  if (!t) return;
  if (e.button === 1) { e.preventDefault(); closeTab(+t.dataset.i); return; }
  switchTab(+t.dataset.i);
});

// ---------------------------------------------------------------- render
let renderToken = 0;
async function render({ restoreScroll = false } = {}) {
  const t = cur();
  const url = t.stack[t.i];
  const p = parse(url);
  const token = ++renderToken;
  renderTabs();
  $("#back").disabled = t.i === 0;
  $("#fwd").disabled = t.i >= t.stack.length - 1;
  setOmni(p);
  closeSuggest();
  try { history.replaceState(null, "", toLocation(url)); } catch {}
  store.save();

  let title = "新しいタブ";
  try {
    if (p.kind === "search") title = await renderSearch(p, token);
    else if (p.kind === "doc") title = await renderDoc(p, token, restoreScroll ? t.scroll[t.i] : null);
    else title = await renderHome(token);
  } catch (e) {
    if (token !== renderToken) return;
    view.innerHTML = `<div class="page err">${e.status === 404 ? "この文書は見つかりませんでした。削除されたか、索引の対象外になった可能性があります。" : "読み込みに失敗しました: " + esc(e.message)}</div>`;
    title = "エラー";
  }
  if (token !== renderToken) return;
  t.title = title;
  document.title = `${title} — Local Browser`;
  renderTabs();
  store.save();
  if (restoreScroll && p.kind !== "doc" && t.scroll[t.i] != null) view.scrollTop = t.scroll[t.i];
}

function setOmni(p) {
  if (document.activeElement === omni) return;
  omni.value = p.kind === "search" ? p.q : p.kind === "doc" ? `lb://doc/${p.id}` : "";
}

// ---- new tab
async function renderHome(token) {
  const [st, recent] = await Promise.all([api("/api/status"), api("/api/recent")]);
  if (token !== renderToken) return;
  const roots = st.roots.map(r => r.split("/").pop()).join(", ");
  view.innerHTML = `<div class="home">
    <div class="logo">${ICON.search.replace(/14/g, 34)}<div>Local <span>Browser</span></div></div>
    <form class="home-search" id="homeForm">${ICON.search}<input id="homeQ" placeholder="ガイドライン、手順書、メモを検索" autocomplete="off"></form>
    <div class="home-meta">${st.docs} 件の文書${roots ? " · " + esc(roots) : ""}</div>
    ${BACKEND?.homeHtml ? BACKEND.homeHtml(st) : ""}
    ${recent.length ? `<h2>最近更新された文書</h2><div class="cards">${recent.map(r => `
      <a class="card" href="${esc(toLocation(docUrl(r.id)))}" data-doc="${esc(r.id)}">
        <div class="t">${esc(r.title)}</div><div class="p">${esc(r.rel)}</div></a>`).join("")}</div>` : ""}
    <div class="kbd-help">
      <kbd>/</kbd> 検索 &nbsp; <kbd>Alt</kbd>+<kbd>T</kbd> 新しいタブ &nbsp; <kbd>Alt</kbd>+<kbd>W</kbd> 閉じる &nbsp;
      <kbd>Alt</kbd>+<kbd>←</kbd><kbd>→</kbd> 戻る・進む &nbsp; <kbd>Alt</kbd>+<kbd>1</kbd>…<kbd>9</kbd> タブ切替<br>
      ⌘ / Ctrl / 中クリックで新しいタブに開く
    </div></div>`;
  const q = $("#homeQ");
  $("#homeForm").addEventListener("submit", e => { e.preventDefault(); if (q.value.trim()) navigate(searchUrl(q.value.trim())); });
  BACKEND?.bindHome?.(view, () => render());
  q.focus();
  return "新しいタブ";
}

// ---- search results
async function renderSearch(p, token) {
  if (!p.q.trim()) return renderHome(token);
  view.innerHTML = `<div class="page"><div class="results"><div class="rs-meta">検索中…</div></div></div>`;
  const r = await api(`/api/search?q=${encodeURIComponent(p.q)}&page=${p.page}${p.exact ? "&exact=1" : ""}`);
  if (token !== renderToken) return;
  const fmtDate = s => new Date(s * 1000).toLocaleDateString("ja-JP", { year: "numeric", month: "short", day: "numeric" });
  // 文書を開いたときの強調は、実際に引いた語で (k8se ではなく k8s)
  const hl = r.mode === "corrected" ? r.corrected : p.q;
  const hits = r.hits.map(h => {
    const rootName = h.root.split("/").pop();
    const crumbs = [rootName, ...h.rel.split("/")].map(esc).join(" › ");
    return `<div class="hit">
      <div class="crumb"><span class="badge">${ICON.doc}</span><span class="path">${crumbs}</span></div>
      <a class="title" href="${esc(toLocation(docUrl(h.id, hl)))}" data-doc="${esc(h.id)}" data-hl="${esc(hl)}">${h.title_html}</a>
      <div class="snip"><span class="date">${fmtDate(h.mtime)} —</span>${h.snippet}</div></div>`;
  }).join("");
  const more = (p.page + 1) * r.per < r.total;
  view.innerHTML = `<div class="page"><div class="results">
    <div class="rs-meta">${r.total} 件 (${r.millis.toFixed(1)} ミリ秒)</div>
    ${r.mode === "corrected" ? `<div class="fix">「<b>${esc(r.corrected)}</b>」の結果を表示しています<br>
      <span class="sub">「${esc(p.q)}」に一致する文書はありません。 <a data-exact>元の語「${esc(p.q)}」のまま検索</a></span></div>` : ""}
    ${r.mode === "loose" ? `<div class="fix">「${esc(p.q)}」のすべての語を含む文書はありません。語の一部が一致する文書を表示しています。</div>` : ""}
    ${r.suggestion ? `<div class="fix">もしかして: <a data-sug>${esc(r.suggestion)}</a></div>` : ""}
    ${hits || `<div class="empty">「${esc(p.q)}」に一致する文書は見つかりませんでした。<ul><li>別のキーワードを試す</li><li>語を減らす (すべての語を含む文書だけが出ます)</li><li>右上のボタンで索引を更新する</li></ul></div>`}
    ${(p.page > 0 || more) ? `<div class="pager">${p.page > 0 ? `<button data-page="${p.page - 1}">‹ 前へ</button>` : ""}${more ? `<button data-page="${p.page + 1}">次へ ›</button>` : ""}</div>` : ""}
  </div></div>`;
  view.querySelectorAll("[data-page]").forEach(b => b.addEventListener("click", () => navigate(searchUrl(p.q, +b.dataset.page, p.exact))));
  view.querySelector("[data-exact]")?.addEventListener("click", () => navigate(searchUrl(p.q, 0, true)));
  view.querySelector("[data-sug]")?.addEventListener("click", () => navigate(searchUrl(r.suggestion)));
  view.scrollTop = 0;
  return `${p.q} - 検索`;
}

// ---- document
async function renderDoc(p, token, scrollTo) {
  const d = await api(`/api/doc/${encodeURIComponent(p.id)}`);
  if (token !== renderToken) return;
  const toc = (d.headings || []).filter(h => h.level >= 2 || d.headings.filter(x => x.level === 1).length > 1);
  const dt = new Date(d.mtime * 1000).toLocaleString("ja-JP");
  const isHtml = d.ext === "html" || d.ext === "htm";
  view.innerHTML = `<div class="page"><div class="docwrap">
    <nav class="toc">${toc.length ? `<div class="h">目次</div>` + toc.map(h => `<a class="l${h.level}" href="#${esc(h.anchor)}" data-anchor="${esc(h.anchor)}">${esc(h.text)}</a>`).join("") : ""}</nav>
    <div>
      <div class="docbar">
        <span class="path">${esc(d.rel)}</span><span>更新 ${esc(dt)}</span><span class="sp"></span>
        <button class="chip" id="copyPath">パスをコピー</button>
        ${BACKEND ? "" : `<button class="chip" id="reveal">Finder で表示</button>`}
      </div>
      ${isHtml
        ? `<iframe class="htmlframe" sandbox="" referrerpolicy="no-referrer"></iframe><div class="note">HTML ファイルはスクリプトを無効にした枠の中で表示しています。</div>`
        : `<article class="content" id="content">${d.html}</article>`}
    </div></div></div>`;

  if (isHtml) {
    $(".htmlframe", view).srcdoc = d.html;
  } else {
    const content = $("#content", view);
    // 相対パスの画像を root 内ファイルとして読む
    content.querySelectorAll("img[src]").forEach(img => {
      const src = img.getAttribute("src");
      if (/^[a-z]+:|^\/\//i.test(src)) return;
      if (BACKEND) BACKEND.image(img, d.id, src);
      else img.src = `/api/file?from=${d.id}&href=${encodeURIComponent(src)}`;
    });
    content.querySelectorAll("a[href]").forEach(a => {
      const href = a.getAttribute("href");
      if (/^https?:/i.test(href)) { a.target = "_blank"; a.rel = "noopener noreferrer"; }
    });
    content.addEventListener("click", e => onContentLink(e, d.id));
    if (p.hl) highlight(content, p.hl);
  }
  $("#copyPath", view).addEventListener("click", async () => { try { await navigator.clipboard.writeText(d.path); toast("パスをコピーしました"); } catch { toast(d.path); } });
  $("#reveal", view)?.addEventListener("click", () => api(`/api/reveal/${d.id}`, { method: "POST" }).catch(e => toast("開けませんでした: " + e.message)));
  view.querySelectorAll(".toc a").forEach(a => a.addEventListener("click", e => { e.preventDefault(); scrollToAnchor(a.dataset.anchor, true); }));
  setupScrollSpy();

  requestAnimationFrame(() => {
    if (scrollTo != null) view.scrollTop = scrollTo;
    else if (p.hash) scrollToAnchor(p.hash, false);
    else { const m = view.querySelector("mark.hl"); if (m) m.scrollIntoView({ block: "center" }); else view.scrollTop = 0; }
  });
  return d.title;
}

function scrollToAnchor(anchor, smooth) {
  const el = document.getElementById(anchor);
  if (el) el.scrollIntoView({ behavior: smooth ? "smooth" : "auto", block: "start" });
}

async function onContentLink(e, fromId) {
  const a = e.target.closest("a[href]");
  if (!a) return;
  const href = a.getAttribute("href");
  if (/^[a-z]+:/i.test(href) || href.startsWith("//")) return; // 外部リンクは本物のブラウザへ
  e.preventDefault();
  if (href.startsWith("#")) { scrollToAnchor(decodeURIComponent(href.slice(1)), true); return; }
  const hash = href.includes("#") ? decodeURIComponent(href.split("#")[1]) : "";
  try {
    const r = await api(`/api/resolve?from=${encodeURIComponent(fromId)}&href=${encodeURIComponent(href)}`);
    if (r.id) navigate(docUrl(r.id, "", hash), { newTabBg: e.metaKey || e.ctrlKey });
    else if (r.file) window.open(r.file, "_blank", "noopener");
  } catch (err) {
    toast(err.status === 403 ? "索引の対象外の場所です" : "リンク先が見つかりません: " + href);
  }
}

function highlight(root, q) {
  const terms = q.split(/\s+/).filter(Boolean).map(t => t.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
  if (!terms.length) return;
  const re = new RegExp(`(${terms.join("|")})`, "gi");
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode: n => n.parentElement.closest("pre, code, script, style") && n.nodeValue.length > 2000 ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT,
  });
  const nodes = [];
  while (walker.nextNode()) if (re.test(walker.currentNode.nodeValue)) nodes.push(walker.currentNode);
  for (const n of nodes) {
    const frag = document.createDocumentFragment();
    n.nodeValue.split(re).forEach((part, i) => {
      if (i % 2) { const m = document.createElement("mark"); m.className = "hl"; m.textContent = part; frag.append(m); }
      else if (part) frag.append(part);
    });
    n.replaceWith(frag);
  }
}

let spy;
function setupScrollSpy() {
  spy?.disconnect();
  const links = [...view.querySelectorAll(".toc a")];
  if (!links.length) return;
  const map = new Map(links.map(a => [a.dataset.anchor, a]));
  const visible = new Set();
  spy = new IntersectionObserver(entries => {
    for (const en of entries) en.isIntersecting ? visible.add(en.target.id) : visible.delete(en.target.id);
    const first = links.find(a => visible.has(a.dataset.anchor));
    if (first) links.forEach(a => a.classList.toggle("on", a === first));
  }, { root: view, rootMargin: "0px 0px -70% 0px" });
  map.forEach((_, id) => { const el = document.getElementById(id); if (el) spy.observe(el); });
}

// in-app link clicks (search results / cards)
view.addEventListener("click", e => {
  const a = e.target.closest("a[data-doc]");
  if (!a) return;
  e.preventDefault();
  navigate(docUrl(a.dataset.doc, a.dataset.hl || ""), { newTabBg: e.metaKey || e.ctrlKey });
});
view.addEventListener("auxclick", e => {
  const a = e.target.closest("a[data-doc]");
  if (!a || e.button !== 1) return;
  e.preventDefault();
  navigate(docUrl(a.dataset.doc, a.dataset.hl || ""), { newTabBg: true });
});

// ---------------------------------------------------------------- omnibox
let sgItems = [], sgSel = 0, sgTimer, sgSeq = 0;
const sugEl = $("#suggest");
function closeSuggest() { sugEl.classList.remove("open"); sgItems = []; }
function drawSuggest() {
  if (!sgItems.length) return closeSuggest();
  sugEl.innerHTML = sgItems.map((it, i) => `<div class="sg${i === sgSel ? " sel" : ""}" data-i="${i}">
    <span class="ic">${it.kind === "search" ? ICON.search : ICON.doc}</span>
    <span class="t">${it.kind === "search" ? `<b>${esc(it.q)}</b> を検索` : esc(it.title)}</span>
    ${it.rel ? `<span class="p">${esc(it.rel)}</span>` : ""}</div>`).join("");
  sugEl.classList.add("open");
}
function omniTarget(text) {
  text = text.trim();
  if (!text) return "lb://newtab";
  if (text.startsWith("lb://")) return text;
  return searchUrl(text);
}
omni.addEventListener("focus", () => setTimeout(() => omni.select(), 0));
omni.addEventListener("input", () => {
  const q = omni.value.trim();
  clearTimeout(sgTimer);
  if (!q || q.startsWith("lb://")) { closeSuggest(); return; }
  sgItems = [{ kind: "search", q }]; sgSel = 0; drawSuggest();
  const my = ++sgSeq;
  sgTimer = setTimeout(async () => {
    try {
      const docs = await api(`/api/suggest?q=${encodeURIComponent(q)}`);
      if (my !== sgSeq || document.activeElement !== omni) return;
      sgItems = [{ kind: "search", q }, ...docs.map(d => ({ kind: "doc", ...d, q }))];
      sgSel = Math.min(sgSel, sgItems.length - 1);
      drawSuggest();
    } catch {}
  }, 110);
});
omni.addEventListener("keydown", e => {
  if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    if (!sgItems.length) return;
    e.preventDefault();
    sgSel = (sgSel + (e.key === "ArrowDown" ? 1 : -1) + sgItems.length) % sgItems.length;
    drawSuggest();
  } else if (e.key === "Enter") {
    e.preventDefault();
    const it = sgItems[sgSel];
    const url = it ? (it.kind === "doc" ? docUrl(it.id, it.q) : searchUrl(it.q)) : omniTarget(omni.value);
    const bg = e.metaKey || e.ctrlKey;
    closeSuggest();
    if (!bg) omni.blur();
    navigate(url, { newTabBg: bg });
  } else if (e.key === "Escape") {
    closeSuggest();
    omni.blur();
    setOmni(parse(cur().stack[cur().i]));
  }
});
omni.addEventListener("blur", () => setTimeout(() => { closeSuggest(); setOmni(parse(cur().stack[cur().i])); }, 150));
sugEl.addEventListener("mousedown", e => {
  const row = e.target.closest(".sg");
  if (!row) return;
  e.preventDefault();
  const it = sgItems[+row.dataset.i];
  closeSuggest();
  omni.blur();
  navigate(it.kind === "doc" ? docUrl(it.id, it.q) : searchUrl(it.q), { newTabBg: e.metaKey || e.ctrlKey });
});
function focusSearch() {
  const hq = $("#homeQ");
  if (hq) hq.focus(); else { omni.focus(); omni.select(); }
}

// ---------------------------------------------------------------- toolbar
$("#back").addEventListener("click", () => go(-1));
$("#fwd").addEventListener("click", () => go(1));
$("#reload").addEventListener("click", () => { saveScroll(); render({ restoreScroll: true }); });
$("#reindex").addEventListener("click", async () => {
  try { const r = await api("/api/reindex", { method: "POST" }); toast(r.started ? "索引の更新を始めました" : "すでに更新中です"); pollStatus(true); }
  catch (e) { toast("失敗しました: " + e.message); }
});

let statusTimer, lastFinished = null;
async function pollStatus(soon = false) {
  clearTimeout(statusTimer);
  try {
    const s = await api("/api/status");
    $("#dot").classList.toggle("busy", s.indexing);
    const pr = s.progress;
    $("#statusLbl").textContent = s.indexing ? (pr?.total ? `更新中… ${pr.done}/${pr.total}` : "更新中…") : `${s.docs} 件`;
    const l = s.last;
    $("#status").title = l ? `前回の更新: 追加 ${l.added} / 変更 ${l.updated} / 削除 ${l.deleted} / 失敗 ${l.failed} (${l.millis}ms)\n${s.roots.join("\n")}` : s.roots.join("\n");
    if (l && lastFinished !== null && l.finished_at !== lastFinished) {
      if (l.error) toast("索引の更新に失敗しました: " + l.error);
      else if (l.added || l.updated || l.deleted) toast(`索引を更新しました (+${l.added} ~${l.updated} -${l.deleted})`);
      // 新しいタブは文書数と最近の文書を出しているので描き直す
      if (l.finished_at !== lastFinished && parse(cur().stack[cur().i]).kind === "newtab") render();
    }
    if (l) lastFinished = l.finished_at;
    statusTimer = setTimeout(pollStatus, s.indexing || soon ? 1000 : 15000);
  } catch {
    $("#statusLbl").textContent = "接続なし";
    $("#dot").classList.add("busy");
    statusTimer = setTimeout(pollStatus, 5000);
  }
}

let toastTimer;
function toast(msg) {
  const el = $("#toast");
  el.textContent = msg;
  el.classList.add("show");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => el.classList.remove("show"), 2200);
}

// ---------------------------------------------------------------- keyboard
// ⌘T / ⌘W は本物のブラウザが先に取るので Alt を使う
document.addEventListener("keydown", e => {
  const typing = /^(INPUT|TEXTAREA)$/.test(document.activeElement?.tagName);
  const k = e.code;
  if (e.altKey && !e.metaKey && !e.ctrlKey) {
    if (k === "KeyT") { e.preventDefault(); newTab(); setTimeout(focusSearch, 0); }
    else if (k === "KeyW") { e.preventDefault(); closeTab(active); }
    else if (k === "KeyR") { e.preventDefault(); $("#reload").click(); }
    else if (k === "ArrowLeft" && !typing) { e.preventDefault(); go(-1); }
    else if (k === "ArrowRight" && !typing) { e.preventDefault(); go(1); }
    else if (/^Digit[1-9]$/.test(k)) { e.preventDefault(); const n = +k.slice(5); switchTab(n === 9 ? tabs.length - 1 : n - 1); }
    else if (k === "BracketRight") { e.preventDefault(); switchTab((active + 1) % tabs.length); }
    else if (k === "BracketLeft") { e.preventDefault(); switchTab((active - 1 + tabs.length) % tabs.length); }
    return;
  }
  if ((e.metaKey || e.ctrlKey) && (e.key === "k" || e.key === "l")) { e.preventDefault(); omni.focus(); omni.select(); return; }
  if (!typing && e.key === "/" && !e.metaKey && !e.ctrlKey) { e.preventDefault(); focusSearch(); }
});

// ---------------------------------------------------------------- boot
(function boot() {
  const saved = store.load();
  if (saved?.tabs?.length) {
    tabs = saved.tabs.map(t => ({ id: seq++, stack: t.stack?.length ? t.stack : ["lb://newtab"], i: Math.min(t.i || 0, (t.stack?.length || 1) - 1), title: t.title || "", scroll: {} }));
    active = Math.min(saved.active || 0, tabs.length - 1);
  } else {
    tabs = [{ id: seq++, stack: ["lb://newtab"], i: 0, title: "新しいタブ", scroll: {} }];
  }
  const fromLoc = fromLocation();
  if (fromLoc && fromLoc !== cur().stack[cur().i]) {
    // ブラウザのサイト内検索などから来たら新しいタブで開く
    const blank = parse(cur().stack[cur().i]).kind === "newtab" && cur().stack.length === 1;
    if (blank) { cur().stack = [fromLoc]; cur().i = 0; }
    else { active = tabs.length - 1; newTab(fromLoc, { activate: false }); active = tabs.length - 1; }
  }
  render();
  pollStatus();
  // モード C は Worker から状態が届く (フォルダの追加・取り込みの進み具合)
  BACKEND?.onEvent?.(() => pollStatus(true));
})();
window.lbToast = toast;
