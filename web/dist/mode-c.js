// モード C: 画面 (lb と同じ app.js) の api() を、サーバーの代わりに Worker へつなぐ。
// フォルダを選ぶ・アクセスを許可し直す部分もここ。
"use strict";
(function () {
  const worker = new Worker("worker.js", { type: "module" });
  let seq = 0;
  const waiting = new Map();
  const listeners = [];
  worker.onmessage = e => {
    const m = e.data;
    if (m.event) { listeners.forEach(f => f(m)); return; }
    const w = waiting.get(m.id);
    if (!w) return;
    waiting.delete(m.id);
    m.ok ? w.resolve(m.value) : w.reject(Object.assign(new Error(m.error), { status: m.status }));
  };
  worker.onerror = e => console.error("[lb] worker", e.message);
  const rpc = (op, args) => new Promise((resolve, reject) => {
    const id = ++seq;
    waiting.set(id, { resolve, reject });
    worker.postMessage({ id, op, args });
  });

  const esc = s => String(s ?? "").replace(/[&<>"']/g, c => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
  const canPick = typeof window.showDirectoryPicker === "function";
  const blobUrls = [];
  const urlOf = blob => { const u = URL.createObjectURL(blob); blobUrls.push(u); if (blobUrls.length > 200) URL.revokeObjectURL(blobUrls.shift()); return u; };

  async function api(path) {
    const u = new URL(path, location.href);
    const q = Object.fromEntries(u.searchParams);
    const p = u.pathname.replace(/^.*\/api\//, "");
    if (p === "status") return rpc("status");
    if (p === "recent") return rpc("recent");
    if (p === "search") return rpc("search", { q: q.q || "", page: +q.page || 0, exact: q.exact === "1" });
    if (p === "suggest") return rpc("suggest", { q: q.q || "" });
    if (p.startsWith("doc/")) return rpc("doc", { id: decodeURIComponent(p.slice(4)) });
    if (p === "resolve") {
      const r = await rpc("resolve", { from: q.from, href: q.href });
      return r.blob ? { file: urlOf(r.blob) } : r;
    }
    if (p === "reindex") return rpc("reindex");
    throw Object.assign(new Error("ブラウザ版では使えません"), { status: 501 });
  }

  async function pick() {
    try {
      const handle = await window.showDirectoryPicker({ id: "local-browser", mode: "read" });
      const r = await rpc("addRoot", { handle });
      window.lbToast?.(r.existed ? `「${r.name}」は追加済みです。読み直します` : `「${r.name}」を追加しました。読み込んでいます…`);
    } catch (e) {
      if (e?.name !== "AbortError") window.lbToast?.("フォルダを開けませんでした: " + e.message);
    }
  }

  // Firefox / Safari: <input webkitdirectory>。選んだときの中身しか読めないので、変わったら選び直す
  function pickFiles() {
    const input = document.createElement("input");
    input.type = "file";
    input.webkitdirectory = true;
    input.multiple = true;
    input.onchange = async () => {
      const list = Array.from(input.files);
      if (!list.length) return;
      await addFiles(list.map(f => ({ path: f.webkitRelativePath || f.name, file: f })));
    };
    input.click();
  }

  /** files: [{path: "フォルダ/中のパス", file: File}] */
  async function addFiles(files) {
    const name = files[0].path.split("/")[0];
    const r = await rpc("addFiles", { name, files });
    window.lbToast?.(`「${r.name}」を読み込んでいます…`);
    return r;
  }

  async function allow() {
    const hs = await rpc("handles");
    let ok = 0;
    for (const { handle } of hs) {
      try { if ((await handle.requestPermission({ mode: "read" })) === "granted") ok++; } catch {}
    }
    if (ok) rpc("reindex");
  }

  const LABEL = { granted: "", prompt: "アクセスの許可が必要", denied: "アクセスが拒否されています", files: "選び直すと最新になります" };

  window.lbBackend = {
    mode: "c",
    api,
    addFiles,
    onEvent: f => listeners.push(f),
    homeHtml(st) {
      const rs = st.rootsInfo || [];
      const needAllow = rs.some(r => r.permission === "prompt");
      return `<div class="roots">
        ${rs.length ? rs.map(r => `<span class="root-chip${r.permission !== "granted" ? " warn" : ""}" title="${esc(LABEL[r.permission] || "")}">
          ${esc(r.name)}${LABEL[r.permission] ? ` <small>· ${esc(LABEL[r.permission])}</small>` : ""}
          <button data-remove-root="${esc(r.name)}" title="一覧から外す (ファイルは消しません)">×</button></span>`).join("") : ""}
        ${needAllow ? `<button class="chip primary" id="lbAllow">フォルダへのアクセスを許可</button>` : ""}
        <button class="chip${rs.length ? "" : " primary"}" id="lbPick">${rs.length ? "フォルダを追加" : "フォルダを選ぶ"}</button>
      </div>
      <div class="home-note">${rs.length ? "" : "検索したいドキュメントのフォルダを選んでください。"}
        ファイルはこのブラウザの中で読むだけで、どこにも送りません。変更を取り込むには、ページを再読み込みしてください (変わったファイルだけ読み直します)。
        ${canPick ? "" : "<br>このブラウザはフォルダを覚えておけないので、変更を取り込むときはフォルダを選び直してください。"}</div>`;
    },
    bindHome(view, rerender) {
      view.querySelector("#lbPick")?.addEventListener("click", () => (canPick ? pick() : pickFiles()));
      view.querySelector("#lbAllow")?.addEventListener("click", () => allow());
      view.querySelectorAll("[data-remove-root]").forEach(b => b.addEventListener("click", async () => {
        await rpc("removeRoot", { name: b.dataset.removeRoot });
        window.lbToast?.(`「${b.dataset.removeRoot}」を一覧から外しました`);
        rerender();
      }));
    },
    async image(img, from, src) {
      try { img.src = urlOf(await rpc("image", { from, href: src })); } catch { img.removeAttribute("src"); img.alt ||= src; }
    },
  };
})();
