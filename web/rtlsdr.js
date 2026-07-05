// RTL2832U + R820T の WebUSB ドライバ（ESモジュール）。
//
// Sandeep Mistry / Google の rtlsdrj(Apache-2.0) の実績あるレジスタ列を、
// ブラウザ WebUSB(navigator.usb) 向けに忠実移植したもの。低IF(3.57MHz)＋実ADC方式。
//
// 使い方：const sdr = await RtlSdr.request();
//         await sdr.open({ frequency, sampleRate, gain });   // gain=null で自動
//         await sdr.readSamples(u8 => decoder.feed(u8));      // 連続コールバック
//         sdr.stop();

const WRITE_FLAG = 0x10;
const BLOCK = { DEMOD: 0x000, USB: 0x100, SYS: 0x200, I2C: 0x600 };
const REG = { SYSCTL: 0x2000, EPA_CTL: 0x2148, EPA_MAXPKT: 0x2158, DEMOD_CTL: 0x3000, DEMOD_CTL_1: 0x300b };
const XTAL_FREQ = 28800000;
const IF_FREQ = 3570000;
const R820T_ADDR = 0x34; // R820T=0x34 / R828D(RTL-SDR Blog V4)=0x74（open時に自動検出）
// R820T のレジスタ読み出しはビット反転で返る
const BIT_REVS = [0x0, 0x8, 0x4, 0xc, 0x2, 0xa, 0x6, 0xe, 0x1, 0x9, 0x5, 0xd, 0x3, 0xb, 0x7, 0xf];
// R820T レジスタ 0x05..0x1f の初期値
const R820T_REGS = [0x83, 0x32, 0x75, 0xc0, 0x40, 0xd6, 0x6c, 0xf5, 0x63, 0x75,
  0x68, 0x6c, 0x83, 0x80, 0x00, 0x0f, 0x00, 0xc0, 0x30, 0x48,
  0xcc, 0x60, 0x00, 0x54, 0xae, 0x4a, 0xc0];
// バンド別マルチプレクサ設定 [freqMHz, open_d(0x17), rf_mux(0x1a), tf_c(0x1b)]
const MUX_CFGS = [
  [0, 0x08, 0x02, 0xdf], [50, 0x08, 0x02, 0xbe], [55, 0x08, 0x02, 0x8b], [60, 0x08, 0x02, 0x7b],
  [65, 0x08, 0x02, 0x69], [70, 0x08, 0x02, 0x58], [75, 0x00, 0x02, 0x44], [90, 0x00, 0x02, 0x34],
  [110, 0x00, 0x02, 0x24], [140, 0x00, 0x02, 0x14], [180, 0x00, 0x02, 0x13], [250, 0x00, 0x02, 0x11],
  [280, 0x00, 0x02, 0x00], [310, 0x00, 0x41, 0x00], [588, 0x00, 0x40, 0x00],
];

export class RtlSdr {
  constructor(device) {
    this.dev = device;
    this.name = device.productName || "RTL-SDR";
    this.xtal = XTAL_FREQ;
    this._shadow = null;
    this._pllLock = false;
    this._tunerAddr = R820T_ADDR; // 検出で上書き
    this._vcoPowerRef = 2;        // R820T=2 / R828D=1
  }

  static async request() {
    const device = await navigator.usb.requestDevice({
      filters: [{ vendorId: 0x0bda, productId: 0x2838 }, { vendorId: 0x0bda, productId: 0x2832 }],
    });
    return new RtlSdr(device);
  }

  // ================= 低レベル USB 制御転送 =================
  async _ctrlIn(value, index, length) {
    const r = await this.dev.controlTransferIn(
      { requestType: "vendor", recipient: "device", request: 0, value, index }, Math.max(8, length));
    if (!r.data || r.status !== "ok") throw new Error("controlTransferIn " + (r.status || "no-data"));
    return new Uint8Array(r.data.buffer, r.data.byteOffset, r.data.byteLength).slice(0, length);
  }
  async _ctrlOut(value, index, data) {
    await this.dev.controlTransferOut(
      { requestType: "vendor", recipient: "device", request: 0, value, index }, data);
  }
  // ブロックレジスタ書き込み（リトルエンディアン）
  async _writeReg(block, reg, value, len) {
    const d = len === 1 ? Uint8Array.of(value & 0xff) : Uint8Array.of(value & 0xff, (value >> 8) & 0xff);
    await this._ctrlOut(reg, block | WRITE_FLAG, d);
  }
  async _readReg(block, reg, len) {
    const b = await this._ctrlIn(reg, block, len);
    return len === 1 ? b[0] : b[0] | (b[1] << 8);
  }
  // デモッドレジスタ書き込み（ビッグエンディアン）＋ダミーリード
  async _writeDemod(page, addr, value, len) {
    const d = len === 1 ? Uint8Array.of(value & 0xff) : Uint8Array.of((value >> 8) & 0xff, value & 0xff);
    await this._ctrlOut((addr << 8) | 0x20, page | WRITE_FLAG, d);
    await this._ctrlIn((0x01 << 8) | 0x20, 0x0a, 1).catch(() => {});
  }
  async _i2cOpen() { await this._writeDemod(1, 0x01, 0x18, 1); }
  async _i2cClose() { await this._writeDemod(1, 0x01, 0x10, 1); }
  // チューナへ I2C 書き込み（1レジスタ）
  async _r820tWriteRaw(reg, val) { await this._ctrlOut(this._tunerAddr, BLOCK.I2C | WRITE_FLAG, Uint8Array.of(reg, val)); }
  // チューナから読み出し（開始レジスタ→len バイト、ビット反転を戻す）
  async _r820tRead(start, len) {
    await this._ctrlOut(this._tunerAddr, BLOCK.I2C | WRITE_FLAG, Uint8Array.of(start));
    const b = await this._ctrlIn(this._tunerAddr, BLOCK.I2C, len);
    for (let i = 0; i < b.length; i++) b[i] = (BIT_REVS[b[i] & 0xf] << 4) | BIT_REVS[b[i] >> 4];
    return b;
  }
  // シャドウを使ったマスク書き込み
  async _r820tWrite(addr, value, mask) {
    let v = value;
    if (mask !== 0xff) v = (this._shadow[addr - 5] & ~mask) | (value & mask);
    this._shadow[addr - 5] = v & 0xff;
    await this._r820tWriteRaw(addr, v & 0xff);
  }
  async _r820tWriteEach(rows) { for (const [a, v, m] of rows) await this._r820tWrite(a, v, m); }

  // ================= 初期化 =================
  async open({ frequency, sampleRate, gain = null }) {
    await this.dev.open();
    if (this.dev.configuration === null) await this.dev.selectConfiguration(1);

    // USB / デモッド初期化
    await this._writeReg(BLOCK.USB, REG.SYSCTL, 0x09, 1);
    await this._writeReg(BLOCK.USB, REG.EPA_MAXPKT, 0x0200, 2);
    await this._writeReg(BLOCK.USB, REG.EPA_CTL, 0x0210, 2);
    await this.dev.claimInterface(0);
    await this._initDemod();

    // 低IF＋チューナ
    await this._i2cOpen();
    await this._detectTuner(); // R820T(0x34) / R828D(0x74) を reg0==0x69 で判定
    const mult = -1 * Math.floor((IF_FREQ * (1 << 22)) / this.xtal);
    await this._writeDemod(1, 0xb1, 0x1a, 1); // ゼロIF 無効
    await this._writeDemod(0, 0x08, 0x4d, 1); // I 側 ADC のみ
    await this._writeDemod(1, 0x19, (mult >> 16) & 0x3f, 1);
    await this._writeDemod(1, 0x1a, (mult >> 8) & 0xff, 1);
    await this._writeDemod(1, 0x1b, mult & 0xff, 1);
    await this._writeDemod(1, 0x15, 0x01, 1); // スペクトル反転
    await this._r820tInit();
    await this._setGain(gain);
    await this._i2cClose();

    await this.setSampleRate(sampleRate);
    await this.setCenterFrequency(frequency);
    await this._resetBuffer();
    this.freq = frequency;
    this.rate = sampleRate;
  }

  async _initDemod() {
    const rows = [
      [BLOCK.SYS, REG.DEMOD_CTL_1, 0x22, 1], [BLOCK.SYS, REG.DEMOD_CTL, 0xe8, 1],
    ];
    for (const [b, r, v, l] of rows) await this._writeReg(b, r, v, l);
    const dm = [
      [1, 0x01, 0x14], [1, 0x01, 0x10], [1, 0x15, 0x00], [1, 0x16, 0x0000, 2], [1, 0x16, 0x00], [1, 0x17, 0x00],
      [1, 0x18, 0x00], [1, 0x19, 0x00], [1, 0x1a, 0x00], [1, 0x1b, 0x00],
      // FIR 係数 0x1c..0x2f
      [1, 0x1c, 0xca], [1, 0x1d, 0xdc], [1, 0x1e, 0xd7], [1, 0x1f, 0xd8], [1, 0x20, 0xe0], [1, 0x21, 0xf2],
      [1, 0x22, 0x0e], [1, 0x23, 0x35], [1, 0x24, 0x06], [1, 0x25, 0x50], [1, 0x26, 0x9c], [1, 0x27, 0x0d],
      [1, 0x28, 0x71], [1, 0x29, 0x11], [1, 0x2a, 0x14], [1, 0x2b, 0x71], [1, 0x2c, 0x74], [1, 0x2d, 0x19],
      [1, 0x2e, 0x41], [1, 0x2f, 0xa5],
      [0, 0x19, 0x05], [1, 0x93, 0xf0], [1, 0x94, 0x0f], [1, 0x11, 0x00], [1, 0x04, 0x00], [0, 0x61, 0x60],
      [0, 0x06, 0x80], [1, 0xb1, 0x1b], [0, 0x0d, 0x83],
    ];
    for (const row of dm) await this._writeDemod(row[0], row[1], row[2], row[3] || 1);
  }

  // ================= R820T / R828D チューナ =================
  // I2C アドレスを走査し、reg0(生値)==0x69 のチューナを検出。R828D は vco_power_ref=1。
  async _detectTuner() {
    for (const a of [0x34, 0x74]) {
      try {
        await this._ctrlOut(a, BLOCK.I2C | WRITE_FLAG, Uint8Array.of(0x00));
        const b = await this._ctrlIn(a, BLOCK.I2C, 1);
        if (b[0] === 0x69) {
          this._tunerAddr = a;
          this._vcoPowerRef = a === 0x74 ? 1 : 2;
          this.tuner = a === 0x74 ? "R828D" : "R820T";
          return;
        }
      } catch (_) { /* stall → 別アドレス */ }
    }
    throw new Error("対応チューナ(R820T/R828D)が見つかりません");
  }

  async _r820tInit() {
    this._shadow = new Uint8Array(R820T_REGS);
    for (let i = 0; i < R820T_REGS.length; i++) await this._r820tWriteRaw(0x05 + i, R820T_REGS[i]);
    // initElectronics
    await this._r820tWriteEach([[0x0c, 0x00, 0x0f], [0x13, 49, 0x3f], [0x1d, 0x00, 0x38]]);
    const filterCap = await this._calibrateFilter(true);
    await this._r820tWriteEach([
      [0x0a, 0x10 | filterCap, 0x1f], [0x0b, 0x6b, 0xef], [0x07, 0x00, 0x80], [0x06, 0x10, 0x30],
      [0x1e, 0x40, 0x60], [0x05, 0x00, 0x80], [0x1f, 0x00, 0x80], [0x0f, 0x00, 0x80], [0x19, 0x60, 0x60],
      [0x1d, 0xe5, 0xc7], [0x1c, 0x24, 0xf8], [0x0d, 0x53, 0xff], [0x0e, 0x75, 0xff], [0x05, 0x00, 0x60],
      [0x06, 0x00, 0x08], [0x11, 0x38, 0x08], [0x17, 0x30, 0x30], [0x0a, 0x40, 0x60], [0x1d, 0x00, 0x38],
      [0x1c, 0x00, 0x04], [0x06, 0x00, 0x40], [0x1a, 0x30, 0x30], [0x1d, 0x18, 0x38], [0x1c, 0x24, 0x04],
      [0x1e, 0x0d, 0x1f], [0x1a, 0x20, 0x30],
    ]);
  }

  async _calibrateFilter(firstTry) {
    await this._r820tWriteEach([[0x0b, 0x6b, 0x60], [0x0f, 0x04, 0x04], [0x10, 0x00, 0x03]]);
    await this._setPll(56000000);
    if (!this._pllLock) throw new Error("PLL がロックできません（フィルタ校正）");
    await this._r820tWriteEach([[0x0b, 0x10, 0x10], [0x0b, 0x00, 0x10], [0x0f, 0x00, 0x04]]);
    const arr = await this._r820tRead(0x00, 5);
    let filterCap = arr[4] & 0x0f;
    if (filterCap === 0x0f) filterCap = 0;
    if (filterCap !== 0 && firstTry) return this._calibrateFilter(false);
    return filterCap;
  }

  async _setMux(freq) {
    const f = freq / 1e6;
    let i = 0;
    for (; i < MUX_CFGS.length - 1; i++) if (f < MUX_CFGS[i + 1][0]) break;
    const c = MUX_CFGS[i];
    await this._r820tWriteEach([
      [0x17, c[1], 0x08], [0x1a, c[2], 0xc3], [0x1b, c[3], 0xff], [0x10, 0x00, 0x0b],
      [0x08, 0x00, 0x3f], [0x09, 0x00, 0x3f],
    ]);
  }

  async _setPll(freq) {
    const pllRef = this.xtal;
    await this._r820tWriteEach([[0x10, 0x00, 0x10], [0x1a, 0x00, 0x0c], [0x12, 0x80, 0xe0]]);
    let divNum = Math.min(6, Math.floor(Math.log(1770000000 / freq) / Math.LN2));
    const mixDiv = 1 << (divNum + 1);
    const rd = await this._r820tRead(0x00, 5);
    const vcoFineTune = (rd[4] & 0x30) >> 4;
    const ref = this._vcoPowerRef; // R820T=2 / R828D=1
    if (vcoFineTune > ref) divNum -= 1; else if (vcoFineTune < ref) divNum += 1;
    await this._r820tWrite(0x10, divNum << 5, 0xe0);
    const vcoFreq = freq * mixDiv;
    const nint = Math.floor(vcoFreq / (2 * pllRef));
    const vcoFra = vcoFreq % (2 * pllRef);
    if (nint > (128 / ref) - 1) { this._pllLock = false; return; }
    const ni = Math.floor((nint - 13) / 4);
    const si = (nint - 13) % 4;
    await this._r820tWrite(0x14, ni + (si << 6), 0xff);
    await this._r820tWrite(0x12, vcoFra === 0 ? 0x08 : 0x00, 0x08);
    const sdm = Math.min(65535, Math.floor((32768 * vcoFra) / pllRef));
    await this._r820tWrite(0x16, (sdm >> 8) & 0xff, 0xff);
    await this._r820tWrite(0x15, sdm & 0xff, 0xff);
    await this._getPllLock(true);
    await this._r820tWrite(0x1a, 0x08, 0x08);
  }

  async _getPllLock(firstTry) {
    const arr = await this._r820tRead(0x00, 3);
    if (arr[2] & 0x40) { this._pllLock = true; return; }
    if (firstTry) { await this._r820tWrite(0x12, 0x60, 0xe0); return this._getPllLock(false); }
    this._pllLock = false;
  }

  async _setGain(gain) {
    if (gain == null) {
      await this._r820tWriteEach([[0x05, 0x00, 0x10], [0x07, 0x10, 0x10], [0x0c, 0x0b, 0x9f]]);
      return;
    }
    let step = gain <= 15
      ? Math.round(1.36 + gain * (1.1118 + gain * (-0.0786 + gain * 0.0027)))
      : Math.round(1.2068 + gain * (0.6875 + gain * (-0.01011 + gain * 0.0001587)));
    step = Math.max(0, Math.min(30, step));
    const lna = Math.floor(step / 2), mixer = Math.floor((step - 1) / 2);
    await this._r820tWriteEach([
      [0x05, 0x10, 0x10], [0x07, 0x00, 0x10], [0x0c, 0x08, 0x9f], [0x05, lna, 0x0f], [0x07, mixer, 0x0f],
    ]);
  }

  // ================= レート/周波数/バッファ =================
  async setSampleRate(rate) {
    let ratio = Math.floor((XTAL_FREQ * (1 << 22)) / rate) & 0x0ffffffc;
    await this._writeDemod(1, 0x9f, (ratio >> 16) & 0xffff, 2);
    await this._writeDemod(1, 0xa1, ratio & 0xffff, 2);
    await this._writeDemod(1, 0x3e, 0x00, 1); // ppm offset high
    await this._writeDemod(1, 0x3f, 0x00, 1); // ppm offset low
    await this._writeDemod(1, 0x01, 0x14, 1);
    await this._writeDemod(1, 0x01, 0x10, 1);
    this.rate = rate;
  }

  async setCenterFrequency(freq) {
    await this._i2cOpen();
    await this._setMux(freq + IF_FREQ);
    await this._setPll(freq + IF_FREQ);
    await this._i2cClose();
    this.freq = freq;
  }

  async _resetBuffer() {
    await this._writeReg(BLOCK.USB, REG.EPA_CTL, 0x0210, 2);
    await this._writeReg(BLOCK.USB, REG.EPA_CTL, 0x0000, 2);
  }

  // ================= バルク読み出し（多重バッファ連続）=================
  // 複数の transferIn を常に in-flight にしてデバイスFIFOを吸い出し続ける。こうしないと
  // onChunk 側の重い処理（同期計算など）で読み取りが一瞬止まった隙に RTL2832 のFIFOが
  // 溢れてサンプルが欠落し、OFDMの連続性が壊れて同期できない。順序は発行順で保つ。
  // transferSize×inflight ぶんの猶予（既定 128KB×32 ≈ 4MB ≈ 2秒）。
  async readSamples(onChunk, { transferSize = 1 << 17, inflight = 32 } = {}) {
    this._running = true;
    const queue = [];
    for (let i = 0; i < inflight && this._running; i++) queue.push(this.dev.transferIn(1, transferSize));
    while (this._running) {
      let r;
      try { r = await queue.shift(); } catch (_) { if (!this._running) break; continue; }
      if (this._running) queue.push(this.dev.transferIn(1, transferSize)); // すぐ補充＝常に N 本
      if (r && r.status === "ok" && r.data && r.data.byteLength) {
        onChunk(new Uint8Array(r.data.buffer, r.data.byteOffset, r.data.byteLength));
      } else if (r && r.status === "stall") {
        await this.dev.clearHalt("in", 1);
      }
    }
    // 残りの in-flight を回収（デバイス解放を確実に）
    await Promise.allSettled(queue);
  }
  stop() { this._running = false; }
}
