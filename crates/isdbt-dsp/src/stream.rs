//! ストリーミング復調器（ライブラリAPI）：`feed(iq_u8) → ts_bytes` を逐次で。
//!
//! CLI（`examples/stream_decode.rs`）と WASM（`isdbt-wasm`）の両方から使う中核。
//! 初期バッファで同期＋整列をロック → 以降は届いたIQを1シンボルずつ復調し、
//! [`crate::viterbi::ViterbiStreaming`] ＋ 逐次バイトデインタ/逆拡散/RS で TSパケットを吐く。

use crate::deinterleave::{data_carrier_indices, freq_deinterleave, TimeDeinterleaver};
use crate::demap::{qpsk_soft, BitDeinterleaverQpsk};
use crate::demod::OfdmDemod;
use crate::equalize::{
    detect_symbol_phase, equalize, estimate_channel, extract_segment, SEGMENT_BIN_OFFSET,
};
use crate::iq::u8_iq_to_complex;
use crate::params::{GuardInterval, FFT_LEN};
use crate::pilots::SegmentPilots;
use crate::rs;
use crate::sync::estimate_sync;
use crate::ts::{best_sync_phase, pack_bits_msb, ByteDeinterleaver, EnergyPrbs, BI_I, BI_M, TSP};
use crate::viterbi::{depuncture, Viterbi, ViterbiStreaming, PUNCTURE_2_3};
use num_complex::Complex32;

const RESET_PERIOD: usize = 64; // 1 OFDMフレーム = 64 RSブロック
const PRBS_INIT: u16 = 0xa9;
const TB_DEPTH: usize = 96;
const BYTE_LATENCY: usize = BI_M * BI_I * (BI_I - 1);
const LOCK_SYMS: usize = 4000;
const COMPACT_AT: usize = 8 * 1024 * 1024;
// 1回の feed/pump で処理する最大シンボル数（≈50msぶん）。呼び出し側が描画を挟めるように分割。
const MAX_SYMS_PER_CALL: usize = 64;

#[derive(Clone, Copy)]
struct Locked {
    gi: GuardInterval,
    cfo: f32,
    sym: usize,
    phase0: usize,
}

/// 初期バッファをバッチ処理して整列パラメータを確定。
fn lock_params(specs: &[Vec<Complex32>], phase0: usize) -> Option<(usize, usize, usize)> {
    let pilots = SegmentPilots::center_1seg();
    let mut tdi = TimeDeinterleaver::new(4);
    let mut bdi = BitDeinterleaverQpsk::new();
    let warmup = tdi.latency() + bdi.latency();
    let mut coded: Vec<f32> = Vec::new();
    for (k, sp) in specs.iter().enumerate() {
        let sym_mod4 = (phase0 + k) % 4;
        let seg = extract_segment(sp, SEGMENT_BIN_OFFSET);
        let h = estimate_channel(&seg, sym_mod4, &pilots);
        let eq = equalize(&seg, &h);
        let data: Vec<Complex32> = data_carrier_indices(sym_mod4, &pilots)
            .into_iter()
            .map(|l| eq[l])
            .collect();
        let fd = freq_deinterleave(&data);
        for v in tdi.push_symbol(&fd) {
            let de = bdi.push(qpsk_soft(v));
            if k >= warmup {
                coded.push(de[1]);
                coded.push(de[0]);
            }
        }
    }
    let restored = depuncture(&coded, &PUNCTURE_2_3);
    let bits = Viterbi::new().decode(&restored[..restored.len() & !1]);
    let bytes = pack_bits_msb(&bits, 0);
    let checker = rs::Checker::new();
    let mut best = (0.0f32, 0usize, 0usize, 0usize);
    for c in 0..BI_I {
        let mut di = ByteDeinterleaver::new();
        let stream: Vec<u8> = bytes[c..]
            .iter()
            .enumerate()
            .filter_map(|(j, &b)| {
                let o = di.push(b);
                (j >= BYTE_LATENCY).then_some(o)
            })
            .collect();
        let (phase, sc) = best_sync_phase(&stream);
        if sc < 0.9 {
            continue;
        }
        let blocks: Vec<&[u8]> = (0..)
            .map(|i| phase + i * TSP)
            .take_while(|&i| i + TSP <= stream.len())
            .map(|i| &stream[i..i + TSP])
            .collect();
        for reset_off in 0..RESET_PERIOD {
            let mut prbs = EnergyPrbs::with_init(PRBS_INIT);
            let (mut ok, mut tot) = (0usize, 0usize);
            for (idx, blk) in blocks.iter().take(128).enumerate() {
                if idx % RESET_PERIOD == reset_off {
                    prbs.reset_to(PRBS_INIT);
                }
                let mut o = vec![blk[0]];
                for &b in &blk[1..TSP] {
                    o.push(b ^ (prbs.clock(8) as u8));
                }
                prbs.clock(8);
                if checker.is_codeword(&o) || rs::decode(&o).is_some() {
                    ok += 1;
                }
                tot += 1;
            }
            let f = ok as f32 / tot.max(1) as f32;
            if f > best.0 {
                best = (f, c, reset_off, phase);
            }
        }
    }
    (best.0 >= 0.5).then_some((best.1, best.2, best.3))
}

/// 逐次パイプライン状態（1シンボル→ TSパケット）。warmup 中は状態だけ進め出力破棄。
struct Pipe {
    pilots: SegmentPilots,
    tdi: TimeDeinterleaver,
    bdi: BitDeinterleaverQpsk,
    depu_pos: usize,
    vit: ViterbiStreaming,
    bdeint: ByteDeinterleaver,
    prbs: EnergyPrbs,
    commutator: usize,
    reset_off: usize,
    block_phase: usize,
    mother: Vec<f32>,
    bit_acc: u8,
    bit_cnt: usize,
    byte_in_idx: usize,
    deint_out_idx: usize,
    block_buf: Vec<u8>,
    block_idx: usize,
    ndec: usize,
    nblk: usize,
}
impl Pipe {
    fn new(commutator: usize, reset_off: usize, block_phase: usize) -> Self {
        Self {
            pilots: SegmentPilots::center_1seg(),
            tdi: TimeDeinterleaver::new(4),
            bdi: BitDeinterleaverQpsk::new(),
            depu_pos: 0,
            vit: ViterbiStreaming::new(TB_DEPTH),
            bdeint: ByteDeinterleaver::new(),
            prbs: EnergyPrbs::with_init(PRBS_INIT),
            commutator,
            reset_off,
            block_phase,
            mother: Vec::new(),
            bit_acc: 0,
            bit_cnt: 0,
            byte_in_idx: 0,
            deint_out_idx: 0,
            block_buf: Vec::with_capacity(TSP),
            block_idx: 0,
            ndec: 0,
            nblk: 0,
        }
    }
    fn warmup(&self) -> usize {
        self.tdi.latency() + self.bdi.latency()
    }

    fn process(&mut self, spec: &[Complex32], phase0: usize, k: usize, out: &mut Vec<u8>) {
        let sym_mod4 = (phase0 + k) % 4;
        let seg = extract_segment(spec, SEGMENT_BIN_OFFSET);
        let h = estimate_channel(&seg, sym_mod4, &self.pilots);
        let eq = equalize(&seg, &h);
        let data: Vec<Complex32> = data_carrier_indices(sym_mod4, &self.pilots)
            .into_iter()
            .map(|l| eq[l])
            .collect();
        let fd = freq_deinterleave(&data);
        let td = self.tdi.push_symbol(&fd);
        let warmup = self.warmup();
        if k < warmup {
            for v in td {
                let _ = self.bdi.push(qpsk_soft(v)); // 状態だけ進める
            }
            return;
        }
        for v in td {
            let de = self.bdi.push(qpsk_soft(v)); // [lsb, msb]
            for kept in [de[1], de[0]] {
                // order=1
                loop {
                    let pat = PUNCTURE_2_3[self.depu_pos % PUNCTURE_2_3.len()];
                    self.depu_pos += 1;
                    if pat == 1 {
                        self.mother.push(kept);
                        break;
                    } else {
                        self.mother.push(0.0);
                    }
                }
            }
        }
        let pairs = self.mother.len() / 2;
        for t in 0..pairs {
            let bit = match self.vit.push(self.mother[2 * t], self.mother[2 * t + 1]) {
                Some(b) => b,
                None => continue,
            };
            self.bit_acc = (self.bit_acc << 1) | (bit & 1);
            self.bit_cnt += 1;
            if self.bit_cnt < 8 {
                continue;
            }
            let byte = self.bit_acc;
            self.bit_acc = 0;
            self.bit_cnt = 0;
            if self.byte_in_idx < self.commutator {
                self.byte_in_idx += 1;
                continue;
            }
            self.byte_in_idx += 1;
            let o = self.bdeint.push(byte);
            if self.deint_out_idx < BYTE_LATENCY + self.block_phase {
                self.deint_out_idx += 1;
                continue;
            }
            self.deint_out_idx += 1;
            self.block_buf.push(o);
            if self.block_buf.len() < TSP {
                continue;
            }
            if self.block_idx % RESET_PERIOD == self.reset_off {
                self.prbs.reset_to(PRBS_INIT);
            }
            let mut ds = Vec::with_capacity(TSP);
            ds.push(self.block_buf[0]);
            for &x in &self.block_buf[1..TSP] {
                ds.push(x ^ (self.prbs.clock(8) as u8));
            }
            self.prbs.clock(8);
            self.block_idx += 1;
            self.nblk += 1;
            if let Some(cw) = rs::decode(&ds) {
                out.extend_from_slice(&cw[..188]);
                self.ndec += 1;
            }
            self.block_buf.clear();
        }
        self.mother.drain(0..pairs * 2);
    }
}

/// 逐次ストリーミング復調器。`feed` にIQのu8バイトを渡すと、生成されたTSバイトを返す。
pub struct StreamingDecoder {
    demod: OfdmDemod,
    buf: Vec<Complex32>,
    pending: Vec<u8>,
    dc: Complex32,
    dc_done: bool,
    cur: usize,
    k: usize,
    locked: Option<Locked>,
    pipe: Option<Pipe>,
    need_init: usize,
    live: bool, // true: ロック直後にバックログを捨ててライブエッジから（A/V同期・低遅延）
}

impl Default for StreamingDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamingDecoder {
    pub fn new() -> Self {
        Self {
            demod: OfdmDemod::new(FFT_LEN),
            buf: Vec::new(),
            pending: Vec::new(),
            dc: Complex32::new(0.0, 0.0),
            dc_done: false,
            cur: 0,
            k: 0,
            locked: None,
            pipe: None,
            need_init: (LOCK_SYMS + 8) * 1280 + 60_000, // 同期+ロックに十分な初期サンプル数
            live: false,
        }
    }

    /// ライブモード：ロック直後にバックログを捨ててライブエッジから復号する（A/V同期・低遅延）。
    /// CLI/ファイル（全復号）では false のまま。
    pub fn set_live(&mut self, live: bool) {
        self.live = live;
    }

    /// これまでにロックできたか。
    pub fn is_locked(&self) -> bool {
        self.locked.is_some()
    }

    /// これまでにRS復号したパケット数と評価ブロック数。
    pub fn stats(&self) -> (usize, usize) {
        self.pipe.as_ref().map(|p| (p.ndec, p.nblk)).unwrap_or((0, 0))
    }

    fn append(&mut self, iq: &[u8]) {
        let mut bytes = core::mem::take(&mut self.pending);
        bytes.extend_from_slice(iq);
        let even = bytes.len() & !1;
        for c in u8_iq_to_complex(&bytes[..even]) {
            self.buf.push(if self.dc_done { c - self.dc } else { c });
        }
        self.pending = bytes[even..].to_vec();
    }

    fn try_lock(&mut self) {
        if self.buf.len() < self.need_init {
            return;
        }
        if !self.dc_done {
            let dc = self.buf.iter().sum::<Complex32>() / self.buf.len() as f32;
            for v in self.buf.iter_mut() {
                *v -= dc;
            }
            self.dc = dc;
            self.dc_done = true;
        }
        let off = (self.buf.len() / 10).min(50_000);
        let est = match estimate_sync(&self.buf[off..], FFT_LEN) {
            Some(e) => e,
            None => return, // データを増やして再試行
        };
        let sym = FFT_LEN + est.guard.cp_len(FFT_LEN);
        let sym0 = off + est.symbol_start;
        self.buf.drain(0..sym0); // buf[0] = symbol 0

        let navail = (self.buf.len() / sym).min(LOCK_SYMS);
        if navail < 200 {
            return;
        }
        let (phase0, _) = detect_symbol_phase(
            &extract_segment(&self.demod.demod_one(&self.buf, 0, est.guard, est.cfo_subcarriers), SEGMENT_BIN_OFFSET),
            &SegmentPilots::center_1seg(),
        );
        let specs: Vec<Vec<Complex32>> = (0..navail)
            .map(|k| self.demod.demod_one(&self.buf, k * sym, est.guard, est.cfo_subcarriers))
            .collect();
        let Some((commutator, reset_off, block_phase)) = lock_params(&specs, phase0) else {
            return; // ロック失敗（品質）。増データで再試行
        };
        self.locked = Some(Locked {
            gi: est.guard,
            cfo: est.cfo_subcarriers,
            sym,
            phase0,
        });
        self.pipe = Some(Pipe::new(commutator, reset_off, block_phase));
        // ライブ：溜まったバックログを捨ててライブエッジ付近から復号（音声が映像に遅れないよう）。
        // 整列（PRBS周期・OFDMフレーム・phase0・バイト整列）を壊さないため、
        // 1フレーム=204シンボル単位で捨てる（204%4=0, 64RSブロック=PRBS1周期）。
        if self.live {
            let keep = 800 * sym; // warmup(500)＋各種レイテンシ＋余裕
            if self.buf.len() > keep {
                let frame = 204 * sym;
                let drop = ((self.buf.len() - keep) / frame) * frame;
                if drop > 0 {
                    self.buf.drain(0..drop);
                }
            }
        }
        self.cur = 0;
        self.k = 0;
    }

    /// IQ u8 を投入し、届いたぶんを**全部**処理して生成TSを返す（CLI/ファイル用・従来動作）。
    pub fn feed(&mut self, iq: &[u8]) -> Vec<u8> {
        self.push(iq);
        self.process(usize::MAX)
    }

    /// IQ u8 を投入するだけ（同期/ロックは試みるが、シンボル処理はしない）。
    /// ライブのワーカーは `push` → `pump` ループで、処理を小分けにして描画を挟む。
    pub fn push(&mut self, iq: &[u8]) {
        self.append(iq);
        if self.locked.is_none() {
            self.try_lock();
            // 未ロック時のバッファ暴走防止：ロックできない弱電界のライブ入力では
            // buf が無限に伸び WASM が OOM(=unreachable) する。最新 need_init 分だけ残す。
            if self.locked.is_none() {
                let cap = self.need_init + self.need_init / 2;
                if self.buf.len() > cap {
                    let drop = self.buf.len() - self.need_init;
                    self.buf.drain(0..drop);
                }
            }
        }
    }

    /// バックログを**最大 `MAX_SYMS_PER_CALL` シンボルだけ**処理して生成TSを返す。
    /// 1回が長時間ブロックしないので、呼び出し側は合間に描画コールバックを走らせられる。
    pub fn pump(&mut self) -> Vec<u8> {
        self.process(MAX_SYMS_PER_CALL)
    }

    /// まだ処理していない（バックログの）シンボル数。
    pub fn backlog_syms(&self) -> usize {
        match self.locked {
            Some(lk) => self.buf.len().saturating_sub(self.cur) / lk.sym,
            None => 0,
        }
    }

    fn process(&mut self, max: usize) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(lk) = self.locked {
            let mut n = 0;
            while self.cur + lk.sym <= self.buf.len() && n < max {
                let spec = self.demod.demod_one(&self.buf, self.cur, lk.gi, lk.cfo);
                self.pipe
                    .as_mut()
                    .unwrap()
                    .process(&spec, lk.phase0, self.k, &mut out);
                self.cur += lk.sym;
                self.k += 1;
                n += 1;
                if self.cur >= COMPACT_AT {
                    self.buf.drain(0..self.cur);
                    self.cur = 0;
                }
            }
        }
        out
    }
}
