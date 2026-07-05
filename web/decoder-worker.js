// ライブ復号ワーカー：メインから生IQを受け取り、WASM復調→TS→H.264→WebCodecs→
// OffscreenCanvas 描画までを全部ワーカー内で行う。これでメインスレッド（WebUSB読み取り）が
// 詰まらず、canvas が実時間で更新される。
import init, { WasmDecoder } from "./pkg/isdbt_wasm.js?v=12";

let dec = null, live = null, locked = false, tsBytes = 0;
const post = (o) => self.postMessage(o);
const log = (m) => post({ type: "log", text: m });

// 既存 Uint8Array に断片配列を連結した新しい Uint8Array を返す。
function concatChunks(existing, chunks) {
  let n = existing.length; for (const a of chunks) n += a.length;
  const nb = new Uint8Array(n); nb.set(existing, 0);
  let o = existing.length; for (const a of chunks) { nb.set(a, o); o += a.length; }
  return nb;
}

// TS(PID0x151 H.264) → AUD境界でAU化 → WebCodecs → OffscreenCanvas
class LiveVideo {
  constructor(canvas) {
    this.canvas = canvas;
    this.ctx = canvas.getContext("2d");
    this.es = new Uint8Array(0);
    this.sps = null; this.pps = null; this.dec = null; this.frames = 0; this.awaitingKey = true;
    this.aus = 0; this.keys = 0;
    // 音声（PID 0x152, ADTS AAC）
    this.aes = new Uint8Array(0);
    this.adec = null; this.aconf = false; this.afrm = 0;
  }
  pushTs(ts) {
    const vadd = [], aadd = [];
    for (let i = 0; i + 188 <= ts.length; i += 188) {
      const p = ts.subarray(i, i + 188);
      if (p[0] !== 0x47) continue;
      const pid = ((p[1] & 0x1f) << 8) | p[2];
      const isV = pid === 0x151, isA = pid === 0x152;
      if (!isV && !isA) continue;
      const pusi = (p[1] >> 6) & 1, afc = (p[3] >> 4) & 3;
      let off = 4;
      if (afc & 2) off += 1 + p[4];
      if (off >= 188) continue;
      if (pusi && off + 9 <= 188 && p[off] === 0 && p[off + 1] === 0 && p[off + 2] === 1) off += 9 + p[off + 8]; // PESヘッダ除去
      if (off < 188) (isV ? vadd : aadd).push(p.subarray(off, 188));
    }
    if (vadd.length) { this.es = concatChunks(this.es, vadd); this._split(); }
    if (aadd.length) { this.aes = concatChunks(this.aes, aadd); this._splitAudio(); }
  }
  // 音声ESから ADTS フレームを切り出して AudioDecoder へ
  _splitAudio() {
    const b = this.aes;
    let i = 0;
    while (i + 7 < b.length) {
      if (!(b[i] === 0xff && (b[i + 1] & 0xf6) === 0xf0)) { i++; continue; } // ADTS同期(layer=00)
      const flen = ((b[i + 3] & 3) << 11) | (b[i + 4] << 3) | ((b[i + 5] >> 5) & 7);
      if (flen < 7) { i++; continue; }
      if (i + flen > b.length) break; // フレーム未達→次へ持ち越し
      this._decodeAudio(b.slice(i, i + flen));
      i += flen;
    }
    this.aes = b.slice(i); // 未完バイトを保持
  }
  _decodeAudio(adts) {
    if (typeof AudioDecoder === "undefined") return;
    if (!this.adec) {
      const sfIdx = (adts[2] >> 2) & 0xf;
      const ch = ((adts[2] & 1) << 2) | ((adts[3] >> 6) & 3);
      const rates = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];
      this.askip = 3; // ミッドストリーム開始のAAC整定過渡（かさかさ）を捨てるフレーム数
      this.adec = new AudioDecoder({
        output: (a) => {
          if (this.askip > 0) { this.askip--; a.close(); return; } // 立ち上がり過渡を破棄
          const chs = [];
          for (let c = 0; c < a.numberOfChannels; c++) {
            const buf = new Float32Array(a.numberOfFrames);
            a.copyTo(buf, { planeIndex: c, format: "f32-planar" });
            chs.push(buf);
          }
          const sr = a.sampleRate; a.close();
          post({ type: "pcm", sr, chs }, chs.map((x) => x.buffer)); // メインへ0コピー
        },
        error: (e) => { log("音声デコードエラー: " + (e.message || e)); },
      });
      // ADTSをそのまま渡せる（Chrome/EdgeのAAC）。configはADTS基底レート。
      this.adec.configure({ codec: "mp4a.40.2", sampleRate: rates[sfIdx] || 24000, numberOfChannels: ch || 2 });
      this.aconf = true;
    }
    try { this.adec.decode(new EncodedAudioChunk({ type: "key", timestamp: this.afrm * 42667, data: adts })); this.afrm++; } catch (_) {}
  }
  _split() {
    const b = this.es; const aud = [];
    for (let i = 0; i + 3 < b.length; i++) if (b[i] === 0 && b[i + 1] === 0 && b[i + 2] === 1 && (b[i + 3] & 0x1f) === 9) aud.push(i);
    if (aud.length < 1) return;
    let prev = 0;
    for (const a of aud) { if (a > prev) { this._decodeAU(b.subarray(prev, a)); prev = a; } }
    this.es = b.slice(prev);
  }
  _decodeAU(au) {
    const sc = [];
    for (let i = 0; i + 3 < au.length; i++) if (au[i] === 0 && au[i + 1] === 0 && au[i + 2] === 1) sc.push(i);
    if (!sc.length) return;
    this.aus++;
    const nals = sc.map((p, k) => au.slice(p + 3, k + 1 < sc.length ? sc[k + 1] : au.length));
    let hasIdr = false;
    for (const nal of nals) { const t = nal[0] & 0x1f; if (t === 7) this.sps = nal; else if (t === 8) this.pps = nal; else if (t === 5) hasIdr = true; }
    if (hasIdr) this.keys++;
    if (!this.dec) {
      if (!this.sps || !this.pps || !hasIdr) return;
      if (typeof VideoDecoder === "undefined") { log("この環境は WebCodecs 非対応です"); return; }
      const sps = this.sps, pps = this.pps;
      const avcc = new Uint8Array(11 + sps.length + 3 + pps.length);
      avcc.set([1, sps[1], sps[2], sps[3], 0xff, 0xe1, (sps.length >> 8) & 0xff, sps.length & 0xff], 0);
      let d = 8; avcc.set(sps, d); d += sps.length;
      avcc[d++] = 1; avcc[d++] = (pps.length >> 8) & 0xff; avcc[d++] = pps.length & 0xff; avcc.set(pps, d); d += pps.length;
      this.dec = new VideoDecoder({
        output: (f) => {
          if (this.canvas.width !== f.displayWidth) this.canvas.width = f.displayWidth;
          if (this.canvas.height !== f.displayHeight) this.canvas.height = f.displayHeight;
          this.ctx.drawImage(f, 0, 0); f.close();
          this.frames++;
          if (this.frames === 1) log("ライブ映像 表示開始（WebCodecs）");
        },
        error: (e) => { log("デコードエラー→復帰: " + (e.message || e)); this._reset(); },
      });
      const codec = "avc1." + [sps[1], sps[2], sps[3]].map((x) => x.toString(16).padStart(2, "0")).join("");
      this.dec.configure({ codec, description: avcc.slice(0, d), optimizeForLatency: true });
      this.awaitingKey = false;
    }
    if (this.awaitingKey) { if (!hasIdr) return; this.awaitingKey = false; }
    const vcl = nals.filter((n) => (n[0] & 0x1f) !== 9);
    let len = 0; for (const n of vcl) len += 4 + n.length;
    const data = new Uint8Array(len); let q = 0;
    for (const n of vcl) {
      data[q] = (n.length >>> 24) & 255; data[q + 1] = (n.length >>> 16) & 255;
      data[q + 2] = (n.length >>> 8) & 255; data[q + 3] = n.length & 255;
      data.set(n, q + 4); q += 4 + n.length;
    }
    // タイムスタンプは投入順で単調に（描画は即時）
    this._pts = (this._pts || 0) + 66000;
    try { this.dec.decode(new EncodedVideoChunk({ type: hasIdr ? "key" : "delta", timestamp: this._pts, data })); } catch (_) {}
  }
  _reset() { try { if (this.dec && this.dec.state !== "closed") this.dec.close(); } catch (_) {} this.dec = null; this.awaitingKey = true; }
}

let gCanvas = null, wasmInited = false, pumping = false, statSeq = 0;
function start() { dec = new WasmDecoder(); dec.setLive(true); live = new LiveVideo(gCanvas); locked = false; tsBytes = 0; }

// 描画コールバックを走らせるための非スロットリングなマクロタスク yield。
const _mc = new MessageChannel();
let _yieldResolve = null;
_mc.port1.onmessage = () => { const r = _yieldResolve; _yieldResolve = null; if (r) r(); };
const yieldTask = () => new Promise((r) => { _yieldResolve = r; _mc.port2.postMessage(0); });

function feedTs(ts) {
  if (ts.length) {
    if (!locked) { locked = true; post({ type: "locked" }); log("同期＋整列ロック → 映像デコード開始"); }
    live.pushTs(ts);
    tsBytes += ts.length;
  }
}

// バックログを小分けで処理し、合間に yield して WebCodecs の出力（描画）を走らせる。
async function pumpLoop() {
  if (pumping || !dec) return;
  pumping = true;
  while (dec && dec.backlog() > 0) {
    feedTs(dec.pump());
    if (((statSeq++) & 7) === 0) post({ type: "stat", frames: live ? live.frames : 0, tsPk: (tsBytes / 188) | 0, aus: live ? live.aus : 0, keys: live ? live.keys : 0, locked });
    await yieldTask(); // ここで描画コールバックが発火する
  }
  pumping = false;
}

self.onmessage = async (e) => {
  const m = e.data;
  if (m.type === "init") {
    if (!wasmInited) { await init(new URL("./pkg/isdbt_wasm_bg.wasm?v=12", import.meta.url)); wasmInited = true; }
    gCanvas = m.canvas; // OffscreenCanvas（1回だけ委譲）
    start();
    log("WASM 復調器（ワーカー）ロード完了 / VideoDecoder=" + (typeof VideoDecoder));
    post({ type: "ready" });
  } else if (m.type === "reset") {
    start(); // 再接続時：canvasは保持しデコーダだけ作り直し
  } else if (m.type === "iq") {
    if (!dec) return;
    dec.push(new Uint8Array(m.buf)); // 投入のみ。処理は pumpLoop が小分けで
    pumpLoop();
  } else if (m.type === "grab") {
    // 検証用：現在のcanvasをPNGで返す（アプリは使わない）
    try {
      const blob = await gCanvas.convertToBlob({ type: "image/png" });
      const ab = await blob.arrayBuffer();
      post({ type: "png", ab, frames: live ? live.frames : 0, aus: live ? live.aus : 0, keys: live ? live.keys : 0, decCreated: !!(live && live.dec), awaitingKey: live ? live.awaitingKey : null }, [ab]);
    } catch (err) { post({ type: "png", err: String(err && err.message || err) }); }
  }
};
