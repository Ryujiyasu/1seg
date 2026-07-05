//! ⑥ TS化：バイトデインターリーブ（Forney）＋ TS同期検出 ＋ エネルギー逆拡散。
//!
//! ⑤Viterbiの情報ビット → バイト化 → **Forneyバイトデインターリーブ**（I=12, M=17）→
//! 204バイトRSブロック（先頭に同期バイト 0x47／先頭パケットは反転 0xB8）→ RS復号（別）→
//! **エネルギー逆拡散**（PRBS）→ 188バイトMPEG-TSパケット。
//!
//! ここでは「0x47/0xB8 が204バイト周期で立つ」ところまで（＝実電波からTS構造が出た証拠）と、
//! 逆拡散PRBSを実装する。RS(204,188)復号は [`crate::rs`]（予定）。
//! 参照：gr-isdbt `byte_deinterleaver_impl.cc` / `energy_descrambler_impl.cc`。

use std::collections::VecDeque;

/// TSパケット（RSブロック）長。
pub const TSP: usize = 204;
/// バイトインターリーブの分岐数。
pub const BI_I: usize = 12;
/// バイトインターリーブの単位遅延（バイト）。204/12 = 17。
pub const BI_M: usize = 17;
/// TS同期バイト。
pub const SYNC: u8 = 0x47;
/// 反転同期バイト（8パケット周期の先頭）。
pub const SYNC_INV: u8 = 0xb8;

/// ビット列を8bitごとにMSB firstでバイト化する。`bit_offset` で開始位相をずらせる。
pub fn pack_bits_msb(bits: &[u8], bit_offset: usize) -> Vec<u8> {
    let b = &bits[bit_offset.min(bits.len())..];
    let n = b.len() / 8;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let mut v = 0u8;
        for j in 0..8 {
            v = (v << 1) | (b[i * 8 + j] & 1);
        }
        out.push(v);
    }
    out
}

/// Forney畳み込みバイトデインターリーバ（I=12, M=17）。
/// 分岐 `k` の遅延 = `M*(I-1-k)`（＝送信側 `M*k` と合わせて総遅延 `M*(I-1)` 一定）。
pub struct ByteDeinterleaver {
    fifos: Vec<VecDeque<u8>>,
    idx: usize,
}

impl Default for ByteDeinterleaver {
    fn default() -> Self {
        Self::new()
    }
}

impl ByteDeinterleaver {
    pub fn new() -> Self {
        Self::with_rev(false)
    }
    /// `rev=false`：分岐kの遅延 `M*(I-1-k)`（標準）。`rev=true`：`M*k`（逆向き）。
    /// 送信側の向きに合わせる必要があるので、実機では両方試して確定する。
    pub fn with_rev(rev: bool) -> Self {
        let fifos = (0..BI_I)
            .map(|k| {
                let d = if rev { k } else { BI_I - 1 - k };
                VecDeque::from(vec![0u8; BI_M * d])
            })
            .collect();
        Self { fifos, idx: 0 }
    }

    /// 1バイト投入し、対応分岐の遅延線先頭を返す（コミュテータは内部カウンタ）。
    pub fn push(&mut self, byte: u8) -> u8 {
        let k = self.idx % BI_I;
        self.idx += 1;
        let f = &mut self.fifos[k];
        f.push_back(byte);
        f.pop_front().unwrap_or(byte) // 遅延0の分岐（k=I-1）はそのまま
    }
}

/// Forney畳み込みバイト**インターリーバ**（送信側, I=12, M=17）。分岐kの遅延 `M*k`。
/// 検証用（[`ByteDeinterleaver`] の対）。
pub struct ByteInterleaver {
    fifos: Vec<VecDeque<u8>>,
    idx: usize,
}
impl Default for ByteInterleaver {
    fn default() -> Self {
        Self::new()
    }
}
impl ByteInterleaver {
    pub fn new() -> Self {
        let fifos = (0..BI_I)
            .map(|k| VecDeque::from(vec![0u8; BI_M * k]))
            .collect();
        Self { fifos, idx: 0 }
    }
    pub fn push(&mut self, byte: u8) -> u8 {
        let k = self.idx % BI_I;
        self.idx += 1;
        let f = &mut self.fifos[k];
        f.push_back(byte);
        f.pop_front().unwrap_or(byte)
    }
}

/// エネルギー逆拡散のPRBS（gr-isdbt準拠：reg=0xa9, 帰還 bit13^bit14, 15bit）。
/// `clock_prbs(8)` で8bitを1バイトにして返す。
pub struct EnergyPrbs {
    reg: u16,
}

impl Default for EnergyPrbs {
    fn default() -> Self {
        Self::new()
    }
}

impl EnergyPrbs {
    pub fn new() -> Self {
        Self::with_init(0xa9)
    }
    /// 初期値を指定して作る（規格解釈のブレを実機で総当たりするため）。
    pub fn with_init(init: u16) -> Self {
        Self { reg: init & 0x7fff }
    }
    pub fn reset(&mut self) {
        self.reg = 0xa9;
    }
    /// 任意の初期値でリセット。
    pub fn reset_to(&mut self, init: u16) {
        self.reg = init & 0x7fff;
    }
    /// `clocks` ビットぶん進め、そのビット列を整数で返す（8なら1バイト）。
    pub fn clock(&mut self, clocks: usize) -> u32 {
        let mut res = 0u32;
        for _ in 0..clocks {
            let feedback = ((self.reg >> 13) ^ (self.reg >> 14)) & 0x1;
            self.reg = ((self.reg << 1) | feedback) & 0x7fff;
            res = (res << 1) | feedback as u32;
        }
        res
    }
}

/// バイト列中で、位相 `p`（0..204）に同期バイト(0x47/0xB8)が周期204で立つ割合。
pub fn sync_score_at(bytes: &[u8], p: usize) -> f32 {
    let mut hit = 0usize;
    let mut tot = 0usize;
    let mut i = p;
    while i < bytes.len() {
        if bytes[i] == SYNC || bytes[i] == SYNC_INV {
            hit += 1;
        }
        tot += 1;
        i += TSP;
    }
    if tot == 0 {
        0.0
    } else {
        hit as f32 / tot as f32
    }
}

/// 最も同期バイトが揃う位相(0..204)とそのスコアを返す。
pub fn best_sync_phase(bytes: &[u8]) -> (usize, f32) {
    (0..TSP)
        .map(|p| (p, sync_score_at(bytes, p)))
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
        .unwrap_or((0, 0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_bits_msb_basic() {
        let bits = [0, 1, 0, 0, 0, 1, 1, 1]; // 0x47
        assert_eq!(pack_bits_msb(&bits, 0), vec![0x47]);
    }

    #[test]
    fn byte_interleave_deinterleave_roundtrip() {
        // 送信側インターリーバ（分岐kの遅延 M*k touch）と本デインターリーバ（M*(11-k)）の
        // 直列は、総遅延 M*I*(I-1)=2244 バイト（=11 TSP）の恒等になるはず。
        // 各分岐は12バイトごとにしか触られないので、遅延は touch数×12。
        let total = BI_M * BI_I * (BI_I - 1);
        let mut tx: Vec<VecDeque<u8>> = (0..BI_I)
            .map(|k| VecDeque::from(vec![0u8; BI_M * k]))
            .collect();
        let mut rx = ByteDeinterleaver::new();
        let n = total + 500;
        let data: Vec<u8> = (0..n).map(|i| (i * 7 + 3) as u8).collect();
        for (i, &d) in data.iter().enumerate() {
            let k = i % BI_I;
            tx[k].push_back(d);
            let t = tx[k].pop_front().unwrap_or(d);
            let out = rx.push(t);
            if i >= total {
                assert_eq!(out, data[i - total], "i={i}: バイト恒等が崩れた");
            }
        }
    }

    #[test]
    fn prbs_is_deterministic_and_periodic() {
        let mut a = EnergyPrbs::new();
        let first: Vec<u32> = (0..10).map(|_| a.clock(8)).collect();
        let mut b = EnergyPrbs::new();
        let again: Vec<u32> = (0..10).map(|_| b.clock(8)).collect();
        assert_eq!(first, again); // 決定的
                                  // 最初の1バイトは非自明
        assert_ne!(first[0], 0);
    }
}
