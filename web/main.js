// 自作ワンセグ復調器 ブラウザ版：IQ → WASM復調器 → MPEG-TS →（ファイル:mpegts.js / ライブ:WebCodecs）。
// ?v= はブラウザキャッシュ破り（更新のたびに上げる）。
import init, { WasmDecoder } from "./pkg/isdbt_wasm.js?v=12";
import { RtlSdr } from "./rtlsdr.js?v=12";

const $ = (id) => document.getElementById(id);
const video = $("video");
const logEl = $("log");
const log = (m) => {
  logEl.textContent += m + "\n";
  logEl.scrollTop = logEl.scrollHeight;
  console.log(m);
};

const CENTER = 497142857; // ch17 関西テレビ（環境に合わせて変更）
const FS = 1015873;
const GAIN = null; // オートゲイン（R828D の AGC が最良＝実機で復号率100%）
const CHUNK = 1 << 18; // 256KB

let wasmReady = init(new URL("./pkg/isdbt_wasm_bg.wasm?v=12", import.meta.url)).then(() => log("WASM 復調器ロード完了"));

// ---- PAT 合成注入 ----
// ISDB-T 1seg の復号TSは PAT(PID0) を欠く（PMTは 0x1FC8）。mpegts.js 等は PAT→PMT が要るので合成する。
function crc32Mpeg(bytes) {
  let crc = 0xffffffff;
  for (const b of bytes) {
    crc ^= b << 24;
    for (let i = 0; i < 8; i++) crc = (crc & 0x80000000 ? (crc << 1) ^ 0x04c11db7 : crc << 1) >>> 0;
  }
  return crc >>> 0;
}
function buildPat(prog, pmtPid, cc) {
  const sec = [0x00, 0xb0, 0x0d, 0x00, 0x01, 0xc1, 0x00, 0x00,
    (prog >> 8) & 0xff, prog & 0xff, 0xe0 | ((pmtPid >> 8) & 0x1f), pmtPid & 0xff];
  const crc = crc32Mpeg(sec);
  sec.push((crc >>> 24) & 0xff, (crc >>> 16) & 0xff, (crc >>> 8) & 0xff, crc & 0xff);
  const pkt = new Uint8Array(188).fill(0xff);
  pkt.set([0x47, 0x40, 0x00, 0x10 | (cc & 0x0f), 0x00, ...sec], 0);
  return pkt;
}
// TS から PMT(PUSI + table_id 0x02) を探して {pmtPid, prog} を返す。
function findPmt(ts) {
  for (let i = 0; i + 188 <= ts.length; i += 188) {
    const p = ts.subarray(i, i + 188);
    if (p[0] !== 0x47 || !((p[1] >> 6) & 1)) continue;
    let off = 4;
    if ((p[3] >> 4) & 2) off += 1 + p[4];
    if (off >= 188) continue;
    const t = off + 1 + p[off];
    if (t < 188 && p[t] === 0x02) {
      const pmtPid = ((p[1] & 0x1f) << 8) | p[2];
      const prog = (p[t + 3] << 8) | p[t + 4];
      return { pmtPid, prog };
    }
  }
  return null;
}
// PAT を周期注入した新しい TS を返す。
function injectPat(ts) {
  const m = findPmt(ts);
  if (!m) return ts;
  const npkt = ts.length / 188;
  const out = new Uint8Array((npkt + Math.ceil(npkt / 20)) * 188);
  let o = 0, cc = 0;
  for (let i = 0; i < npkt; i++) {
    if (i % 20 === 0) { out.set(buildPat(m.prog, m.pmtPid, cc), o); o += 188; cc = (cc + 1) & 0x0f; }
    out.set(ts.subarray(i * 188, i * 188 + 188), o); o += 188;
  }
  return out.subarray(0, o);
}

// ---- 再生（ファイル：mpegts.js + <video> / ライブ：WebCodecs + <canvas>）----
let player = null;
function playBlob(tsBytes) {
  video.style.display = "";        // ファイルは<video>で
  $("cv").style.display = "none";
  if (player) { player.destroy(); player = null; }
  const url = URL.createObjectURL(new Blob([tsBytes], { type: "video/mp2t" }));
  player = mpegts.createPlayer({ type: "mpegts", url });
  player.attachMediaElement(video);
  player.load();
  video.play().catch(() => {});
}

// ---- ライブ再生：復号TS(PID0x151 H.264) → WebCodecs → canvas ----
// ブラウザMSEはTSを直接扱えず、mpegts.jsはpush給餌APIも持たない。そこで復号TSから
// H.264を取り出し、WebCodecs(VideoDecoder)で直接デコードしてcanvasに描く。
// ※このストリームは1つのPESに複数アクセスユニットを AUD(NALタイプ9) 区切りで詰めるため、
//   AU境界は PUSI ではなく AUD で切る（PUSIはPESヘッダ除去にのみ使う）。
class LiveVideo {
  constructor(canvas) {
    this.canvas = canvas;
    this.ctx = canvas.getContext("2d");
    this.es = new Uint8Array(0); // PID0x151 の連続ES（未確定AUを保持）
    this.sps = null;
    this.pps = null;
    this.dec = null;
    this.frames = 0;
    this.awaitingKey = true; // 起動時/エラー後はキー(IDR)から
  }
  // 復号TSバイトを投入
  pushTs(ts) {
    const add = [];
    for (let i = 0; i + 188 <= ts.length; i += 188) {
      const p = ts.subarray(i, i + 188);
      if (p[0] !== 0x47) continue;
      if ((((p[1] & 0x1f) << 8) | p[2]) !== 0x151) continue; // 映像PIDのみ
      const pusi = (p[1] >> 6) & 1, afc = (p[3] >> 4) & 3;
      let off = 4;
      if (afc & 2) off += 1 + p[4];
      if (off >= 188) continue;
      if (pusi && off + 9 <= 188 && p[off] === 0 && p[off + 1] === 0 && p[off + 2] === 1) off += 9 + p[off + 8]; // PESヘッダ除去
      if (off < 188) add.push(p.subarray(off, 188));
    }
    if (!add.length) return;
    let n = this.es.length; for (const a of add) n += a.length;
    const nb = new Uint8Array(n); nb.set(this.es, 0);
    let o = this.es.length; for (const a of add) { nb.set(a, o); o += a.length; }
    this.es = nb;
    this._split();
  }
  // ESを AUD(00 00 01, NALタイプ9) 境界でアクセスユニットに切り出す
  _split() {
    const b = this.es;
    const aud = [];
    for (let i = 0; i + 3 < b.length; i++) if (b[i] === 0 && b[i + 1] === 0 && b[i + 2] === 1 && (b[i + 3] & 0x1f) === 9) aud.push(i);
    if (aud.length < 1) return; // 境界待ち
    let prev = 0;
    for (const a of aud) { if (a > prev) { this._decodeAU(b.subarray(prev, a)); prev = a; } }
    this.es = b.slice(prev); // 末尾（未完AU）は次へ持ち越し
  }
  _decodeAU(au) {
    const sc = [];
    for (let i = 0; i + 3 < au.length; i++) if (au[i] === 0 && au[i + 1] === 0 && au[i + 2] === 1) sc.push(i);
    if (!sc.length) return;
    const nals = sc.map((p, k) => au.slice(p + 3, k + 1 < sc.length ? sc[k + 1] : au.length));
    let hasIdr = false;
    for (const nal of nals) {
      const t = nal[0] & 0x1f;
      if (t === 7) this.sps = nal; else if (t === 8) this.pps = nal; else if (t === 5) hasIdr = true;
    }
    if (!this.dec) {
      if (!this.sps || !this.pps || !hasIdr) return; // 綺麗なIDRを待つ
      if (typeof VideoDecoder === "undefined") { log("この環境は WebCodecs 非対応です"); return; }
      const sps = this.sps, pps = this.pps;
      const avcc = new Uint8Array(11 + sps.length + 3 + pps.length);
      avcc.set([1, sps[1], sps[2], sps[3], 0xff, 0xe1, (sps.length >> 8) & 0xff, sps.length & 0xff], 0);
      let d = 8; avcc.set(sps, d); d += sps.length;
      avcc[d++] = 1; avcc[d++] = (pps.length >> 8) & 0xff; avcc[d++] = pps.length & 0xff; avcc.set(pps, d); d += pps.length;
      this.dec = new VideoDecoder({
        output: (f) => {
          this.canvas.width = f.displayWidth; this.canvas.height = f.displayHeight;
          this.ctx.drawImage(f, 0, 0); f.close();
          if (this.frames++ === 0) log("ライブ映像 表示開始（WebCodecs）");
        },
        error: (e) => { log("デコードエラー→復帰: " + (e.message || e)); this._reset(); },
      });
      const codec = "avc1." + [sps[1], sps[2], sps[3]].map((x) => x.toString(16).padStart(2, "0")).join("");
      this.dec.configure({ codec, description: avcc.slice(0, d), optimizeForLatency: true });
      this.awaitingKey = false;
    }
    if (this.awaitingKey) { if (!hasIdr) return; this.awaitingKey = false; } // 復帰は次のIDRから
    const vcl = nals.filter((n) => (n[0] & 0x1f) !== 9); // AUD除外（1AU=1ピクチャ）
    let len = 0; for (const n of vcl) len += 4 + n.length;
    const data = new Uint8Array(len); let q = 0;
    for (const n of vcl) {
      data[q] = (n.length >>> 24) & 255; data[q + 1] = (n.length >>> 16) & 255;
      data[q + 2] = (n.length >>> 8) & 255; data[q + 3] = n.length & 255;
      data.set(n, q + 4); q += 4 + n.length;
    }
    try {
      this.dec.decode(new EncodedVideoChunk({ type: hasIdr ? "key" : "delta", timestamp: this.frames * 66000, data }));
    } catch (_) { /* キー待ち等は無視 */ }
  }
  // デコーダがエラー状態に落ちたら作り直し、次のIDRから復帰
  _reset() {
    try { if (this.dec && this.dec.state !== "closed") this.dec.close(); } catch (_) {}
    this.dec = null;
    this.awaitingKey = true;
  }
}

// ---- ① ファイルモード（確実）----
$("file").addEventListener("change", async (e) => {
  const f = e.target.files[0];
  if (!f) return;
  await wasmReady;
  log(`ファイル: ${f.name} (${(f.size / 1e6).toFixed(1)} MB) を復号中…`);
  const buf = new Uint8Array(await f.arrayBuffer());
  const dec = new WasmDecoder();
  const parts = [];
  let locked = false;
  for (let off = 0; off < buf.length; off += CHUNK) {
    const ts = dec.feed(buf.subarray(off, Math.min(off + CHUNK, buf.length)));
    if (ts.length) {
      parts.push(ts);
      if (!locked) { locked = true; log("同期＋整列ロック → TS生成中…"); }
    }
    if (off % (CHUNK * 40) === 0) await new Promise((r) => setTimeout(r)); // UIを止めない
  }
  const total = parts.reduce((n, p) => n + p.length, 0);
  const ts = new Uint8Array(total);
  let p = 0;
  for (const part of parts) { ts.set(part, p); p += part.length; }
  log(`復号完了: ${(ts.length / 188) | 0} TSパケット (${(ts.length / 1e3).toFixed(0)} KB)`);
  const tsp = injectPat(ts); // PAT(PID0)を合成注入して標準プレーヤで再生可能に
  log(`PAT注入 → ${(tsp.length / 188) | 0} パケット。再生開始`);
  enableDownload(tsp);
  playBlob(tsp);
});

// ---- ライブ音声：ワーカーが送るPCMを AudioWorklet(リングバッファ)で連続再生 ----
// チャンクごとに BufferSource を並べるとジッタ/境界でプツプツ切れるので、音声スレッドで
// サンプル単位に連結して出す AudioWorklet を使う。
let audioCtx = null, pcmNode = null, audioReady = null;
async function initAudio() {
  if (audioCtx) { await audioCtx.resume().catch(() => {}); return; }
  audioCtx = new (self.AudioContext || self.webkitAudioContext)();
  audioReady = audioCtx.audioWorklet.addModule("./pcm-worklet.js?v=12")
    .then(() => {
      pcmNode = new AudioWorkletNode(audioCtx, "pcm-player", { outputChannelCount: [2] });
      pcmNode.connect(audioCtx.destination);
    })
    .catch((e) => log("音声初期化エラー: " + (e.message || e)));
  await audioReady;
  await audioCtx.resume().catch(() => {});
}
function playPcm(_sr, chs) {
  if (pcmNode && chs && chs.length) pcmNode.port.postMessage(chs, chs.map((c) => c.buffer));
}

// ---- ② WebUSB ライブモード ----
// メインスレッドは「WebUSB読み取り→IQをワーカーへ移送」だけ。復調・H.264/AACデコード・canvas描画は
// すべてワーカー(decoder-worker.js)で行い、メインを詰まらせず更新。音声PCMだけ受けて Web Audio で再生。
let liveWorker = null, liveOffscreen = false;
$("usb").addEventListener("click", async () => {
  await wasmReady;
  try {
    log("RTL-SDR を選択…（デバイス許可ダイアログ）");
    const sdr = await RtlSdr.request();
    await sdr.open({ frequency: CENTER, sampleRate: FS, gain: GAIN });
    log(`接続: ${sdr.name} [${sdr.tuner}] / ${CENTER / 1e6}MHz fs=${FS} gain=${GAIN === null ? "auto" : GAIN}`);
    video.style.display = "none";
    const cv = $("cv"); cv.style.display = "block";
    // クリック＝ユーザー操作なので音声を許可（AudioWorklet 初期化/再開）
    await initAudio();

    let locked = false;
    if (!liveWorker) {
      liveWorker = new Worker("./decoder-worker.js?v=12", { type: "module" });
      liveWorker.onmessage = (e) => {
        const d = e.data;
        if (d.type === "log") log(d.text);
        else if (d.type === "locked") locked = true;
        else if (d.type === "pcm") playPcm(d.sr, d.chs);
        else if (d.type === "stat" && d.locked) log(`受信TS ${d.tsPk} パケット / ${d.frames} フレーム描画`);
      };
    }
    if (!liveOffscreen) {
      const off = cv.transferControlToOffscreen(); // canvas制御をワーカーへ委譲（1回だけ）
      liveWorker.postMessage({ type: "init", canvas: off }, [off]);
      liveOffscreen = true;
    } else {
      liveWorker.postMessage({ type: "reset" }); // 再接続：デコーダだけ作り直し
    }

    let seq = 0, bytesIn = 0, nextHint = 12e6;
    log("受信中…（重い処理は別スレッド。ロックまで数秒）");
    await sdr.readSamples((iqU8) => {
      bytesIn += iqU8.length;
      // std ヒント（未ロック時のみ、軽量）
      if (!locked && bytesIn >= nextHint) {
        nextHint += 12e6;
        let sum = 0, sum2 = 0, clip = 0; const N = iqU8.length;
        for (let i = 0; i < N; i++) { const v = iqU8[i]; sum += v; sum2 += v * v; if (v === 0 || v === 255) clip++; }
        const mean = sum / N, std = Math.sqrt(Math.max(0, sum2 / N - mean * mean));
        const diag = std < 5 ? "★信号ほぼ無し/DC → チューナ未同調かアンテナ未接続"
          : std > 70 ? "★過大 → 減衰が必要" : "信号あり（同期探索中）";
        log(`同期探索中 ${(bytesIn / 1e6) | 0}MB / std=${std.toFixed(1)} mean=${mean.toFixed(0)} clip=${(clip / N * 100).toFixed(1)}% ${diag}`);
      }
      // 生IQをワーカーへ0コピー移送（コピー→transfer）
      const buf = iqU8.slice().buffer;
      liveWorker.postMessage({ type: "iq", buf, seq: seq++ }, [buf]);
    });
  } catch (err) {
    log("WebUSB エラー: " + (err && err.message || err));
  }
});

// ---- ダウンロード ----
function enableDownload(ts) {
  const btn = $("dl");
  btn.disabled = false;
  btn.onclick = () => {
    const a = document.createElement("a");
    a.href = URL.createObjectURL(new Blob([ts], { type: "video/mp2t" }));
    a.download = "kantele.ts";
    a.click();
  };
}
