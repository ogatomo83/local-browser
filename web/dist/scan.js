// 選んだフォルダの中を歩いて、取り込み元の一覧を作る。中身はまだ読まない (更新日時とサイズだけ)。
// lb の source.rs と同じ規則: 隠しファイル・node_modules などは飛ばす、8MB より大きいものは読まない。

export const SKIP_DIRS = new Set(["node_modules", "target", "dist", "build", "vendor", "__pycache__"]);
export const MAX_FILE_SIZE = 8 << 20;

const extOf = name => (name.includes(".") ? name.split(".").pop().toLowerCase() : "");

function item(key, name, file, open) {
  return {
    key,
    name,
    version: `${file.lastModified}-${file.size}`,
    mtime: Math.floor(file.lastModified / 1000),
    size: file.size,
    open,
  };
}

/** FileSystemDirectoryHandle (showDirectoryPicker) を歩く */
export async function walkHandle(dir, root, exts, out = [], prefix = "") {
  for await (const [name, h] of dir.entries()) {
    if (name.startsWith(".")) continue;
    if (h.kind === "directory") {
      if (SKIP_DIRS.has(name)) continue;
      await walkHandle(h, root, exts, out, `${prefix}${name}/`);
    } else if (exts.includes(extOf(name))) {
      let file;
      try { file = await h.getFile(); } catch { continue; }
      if (file.size > MAX_FILE_SIZE) continue;
      const rel = prefix + name;
      out.push(item(`${root}/${rel}`, rel, file, async () => file));
    }
  }
  return out;
}

/** <input webkitdirectory> で選んだファイル ({path: "フォルダ/中のパス", file}) */
export function fromFiles(root, files, exts) {
  const out = [];
  for (const { path, file } of files) {
    const parts = path.split("/");
    const rel = parts.slice(1).join("/");
    if (!rel || parts.some(p => p.startsWith(".")) || parts.slice(1, -1).some(p => SKIP_DIRS.has(p))) continue;
    if (!exts.includes(extOf(rel)) || file.size > MAX_FILE_SIZE) continue;
    out.push(item(`${root}/${rel}`, rel, file, async () => file));
  }
  return out;
}

/** フォルダの中の相対パスを解決する (../ で外へは出さない) */
export function resolvePath(fromRel, href) {
  href = href.split(/[?#]/)[0];
  try { href = decodeURIComponent(href); } catch {}
  const base = href.startsWith("/") ? [] : fromRel.split("/").slice(0, -1);
  for (const p of href.replace(/^\/+/, "").split("/")) {
    if (p === "..") { if (!base.length) return null; base.pop(); }
    else if (p && p !== ".") base.push(p);
  }
  return base.join("/");
}

export async function fileInHandle(dir, rel) {
  const parts = rel.split("/");
  for (const p of parts.slice(0, -1)) dir = await dir.getDirectoryHandle(p);
  return (await dir.getFileHandle(parts.at(-1))).getFile();
}
