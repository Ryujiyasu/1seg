// PCM再生用 AudioWorklet：メインから送られる Float32 の音声チャンクをキューに貯め、
// 音声スレッドで連続的（サンプル単位でシームレス）に出力する。チャンクごとに
// AudioBufferSourceNode を並べる方式のプツプツ（境界クリック/アンダーラン）を避ける。
class PcmPlayer extends AudioWorkletProcessor {
  constructor() {
    super();
    this.q = [];            // [Float32Array per channel] の待ち行列
    this.cur = null; this.pos = 0;
    this.buffered = 0;      // 貯まっているサンプル数
    this.started = false;   // 初期ジッタバッファを貯めてから再生開始
    this.prebuffer = sampleRate * 0.35; // 立ち上がり安定のため 0.35秒貯めてから開始
    this.maxBuffer = sampleRate * 1.0; // 安全網：遅延が1秒超なら古い音を捨てて追従
    this.port.onmessage = (e) => {
      this.q.push(e.data); this.buffered += e.data[0].length;
      while (this.buffered > this.maxBuffer && this.q.length > 1) {
        const d = this.q.shift(); this.buffered -= d[0].length;
      }
    };
  }
  process(_inputs, outputs) {
    const out = outputs[0];
    const nch = out.length, n = out[0].length;
    if (!this.started) {
      if (this.buffered < this.prebuffer) return true; // まだ貯める（出力は無音）
      this.started = true;
    }
    for (let i = 0; i < n; i++) {
      if (!this.cur || this.pos >= this.cur[0].length) { this.cur = this.q.shift() || null; this.pos = 0; }
      if (this.cur) {
        for (let c = 0; c < nch; c++) out[c][i] = (this.cur[c] || this.cur[0])[this.pos];
        this.pos++; this.buffered--;
      } else {
        for (let c = 0; c < nch; c++) out[c][i] = 0; // アンダーラン→無音（クリックにならない）
      }
    }
    return true;
  }
}
registerProcessor("pcm-player", PcmPlayer);
