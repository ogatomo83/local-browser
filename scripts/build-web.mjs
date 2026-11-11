// モード C の静的サイトを web/dist/ に組み立てる。
// 画面は lb と同じ host/ui/index.html から作る (インラインのスクリプトを app.js に出して、CSP で 'self' 以外を止める)。
//   node scripts/build-web.mjs <lb_web.wasm>
import { readFileSync, writeFileSync, mkdirSync, copyFileSync, readdirSync, rmSync } from "node:fs";
import { join } from "node:path";

const wasm = process.argv[2];
const out = "web/dist";
rmSync(out, { recursive: true, force: true });
mkdirSync(out, { recursive: true });

const html = readFileSync("host/ui/index.html", "utf8");
const m = /<script>\n([\s\S]*?)<\/script>/.exec(html);
if (!m) throw new Error("host/ui/index.html にスクリプトがありません");

// 外への通信 (connect-src) は自分のオリジンだけ。wasm と Worker と blob: の画像だけ許す
const CSP = [
  "default-src 'self'",
  "script-src 'self' 'wasm-unsafe-eval'",
  "style-src 'self' 'unsafe-inline'",
  "img-src 'self' data: blob:",
  "connect-src 'self'",
  "worker-src 'self'",
  "frame-src 'none'",
  "object-src 'none'",
  "base-uri 'none'",
  "form-action 'none'",
].join("; ");

const page = html
  .replace('<meta charset="utf-8">', `<meta charset="utf-8">\n<meta http-equiv="Content-Security-Policy" content="${CSP}">\n<meta name="referrer" content="no-referrer">`)
  .replace("<title>Local Browser</title>", "<title>Local Browser</title>\n<meta name=\"description\" content=\"フォルダを選ぶだけで、手元のドキュメントをブラウザの中だけで検索する\">")
  .replace(m[0], '<script src="mode-c.js"></script>\n<script src="app.js"></script>');
writeFileSync(join(out, "index.html"), page);
writeFileSync(join(out, "app.js"), "// 生成物: host/ui/index.html のスクリプト (scripts/build-web.mjs)\n" + m[1]);
for (const f of readdirSync("web/www")) copyFileSync(join("web/www", f), join(out, f));
copyFileSync(wasm, join(out, "lb.wasm"));
console.log(`web/dist: ${readdirSync(out).join(", ")}`);
