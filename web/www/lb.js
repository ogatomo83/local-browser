// lb-web (wasm) を呼ぶ薄い包み。Worker でも Node でも動く (DOM を使わない)。
// やりとりは call(命令, バイト列) の 1 本。結果は JSON (オブジェクト) か Uint8Array。

const enc = new TextEncoder();
const dec = new TextDecoder();

export class Lb {
  static async create(wasmBytes) {
    // import は 1 つもない (wasm は外の世界に触れない)
    const { instance } = await WebAssembly.instantiate(wasmBytes, {});
    const lb = new Lb(instance.exports);
    lb.build = await sha256(wasmBytes);
    return lb;
  }

  constructor(x) { this.x = x; }

  call(cmd, data) {
    const c = enc.encode(JSON.stringify(cmd));
    const d = data ? (data instanceof Uint8Array ? data : new Uint8Array(data)) : new Uint8Array(0);
    const cp = this.put(c), dp = this.put(d);
    let code;
    try { code = this.x.lb_call(cp, c.length, dp, d.length); }
    finally { this.x.lb_free(cp, c.length); this.x.lb_free(dp, d.length); }
    // memory.buffer は伸びると差し替わるので、呼んだあとに取り直す
    const out = new Uint8Array(this.x.memory.buffer, this.x.lb_out_ptr(), this.x.lb_out_len()).slice();
    if (code === 0) return JSON.parse(dec.decode(out));
    if (code === 1) return out;
    throw new Error(dec.decode(out));
  }

  put(bytes) {
    const p = this.x.lb_alloc(bytes.length);
    new Uint8Array(this.x.memory.buffer, p, bytes.length).set(bytes);
    return p;
  }
}

export async function sha256(bytes) {
  const h = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
  return Array.from(h.slice(0, 12), b => b.toString(16).padStart(2, "0")).join("");
}
