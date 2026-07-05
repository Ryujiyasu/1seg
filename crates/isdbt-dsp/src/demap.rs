//! ④終段：QPSKソフトデマップ ＋ ビットデインターリーブ。
//!
//! - **デマップ**：等化済みキャリアを符号ビットのソフト値にする。gr-isdbt のハード判定
//!   `(re<0)<<1 | (im<0)`（MSB=re符号, LSB=im符号）に合わせ、ソフトは
//!   `msb = -re`, `lsb = -im`（正で bit=1）。
//! - **ビットデインターリーブ**：QPSKは bit位置ごとに遅延 `{0, 120}`（キャリアストリーム
//!   上の単一遅延線）。出力 bit b は「`delay[b]` キャリア前のシンボルの bit b」。
//!   gr-isdbt `bit_deinterleaver_impl.cc` 準拠。

use num_complex::Complex32;
use std::collections::VecDeque;

/// QPSKのビット遅延（LSB, MSB）。
pub const QPSK_DELAY: [usize; 2] = [0, 120];
/// 最大遅延（デインターリーバの保持深さ）。
pub const QPSK_MAX_DELAY: usize = 120;

/// QPSKソフトデマップ：1キャリア → 符号ビット2つのソフト値 `[lsb, msb]`。
/// `lsb = -im`, `msb = -re`（正のとき bit=1）。
#[inline]
pub fn qpsk_soft(c: Complex32) -> [f32; 2] {
    // 非有限（等化で H≈0 → Y/H が Inf/NaN。ライブの弱搬送波で発生）は erasure(0) に。
    // 併せて振幅をクランプし、Viterbi のメトリクス暴走・NaN 伝播を防ぐ。
    let s = |x: f32| if x.is_finite() { x.clamp(-8.0, 8.0) } else { 0.0 };
    [s(-c.im), s(-c.re)]
}

/// QPSKビットデインターリーバ（ソフト値）。キャリアを順に流し込む単一遅延線。
pub struct BitDeinterleaverQpsk {
    /// 過去シンボルの soft `[lsb, msb]`（front が最新）。
    hist: VecDeque<[f32; 2]>,
}

impl Default for BitDeinterleaverQpsk {
    fn default() -> Self {
        Self::new()
    }
}

impl BitDeinterleaverQpsk {
    pub fn new() -> Self {
        // gr-isdbt と同じく 120 個のゼロで初期化。
        let mut hist = VecDeque::with_capacity(QPSK_MAX_DELAY + 1);
        for _ in 0..QPSK_MAX_DELAY {
            hist.push_back([0.0, 0.0]);
        }
        Self { hist }
    }

    /// 1キャリアのソフト `[lsb, msb]` を投入し、デインターリーブ後の `[lsb, msb]` を返す。
    /// `out[b] = (delay[b]キャリア前のシンボル)[b]`。
    pub fn push(&mut self, sym: [f32; 2]) -> [f32; 2] {
        self.hist.push_front(sym);
        let lsb = self.hist[QPSK_DELAY[0]][0]; // delay 0
        let msb = self.hist[QPSK_DELAY[1]][1]; // delay 120
        if self.hist.len() > QPSK_MAX_DELAY + 1 {
            self.hist.pop_back();
        }
        [lsb, msb]
    }

    /// 全キャリアの遅延が揃う総レイテンシ（=最大遅延 120）。
    pub fn latency(&self) -> usize {
        QPSK_MAX_DELAY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qpsk_soft_signs_match_hard_rule() {
        // ハード規則: (re<0)<<1 | (im<0)。ソフトの符号が一致するか。
        let cases = [
            (Complex32::new(0.7, 0.7), 0b00),  // re>0,im>0 → msb0,lsb0
            (Complex32::new(-0.7, 0.7), 0b10), // re<0 → msb1
            (Complex32::new(0.7, -0.7), 0b01), // im<0 → lsb1
            (Complex32::new(-0.7, -0.7), 0b11),
        ];
        for (c, hard) in cases {
            let s = qpsk_soft(c);
            let lsb = u8::from(s[0] > 0.0);
            let msb = u8::from(s[1] > 0.0);
            assert_eq!((msb << 1) | lsb, hard, "c={c:?}");
        }
    }

    #[test]
    fn bit_deinterleave_constant_latency_identity() {
        // TX側インターリーブ（bit0を120遅延, bit1を0遅延）と RX側デインターリーブ
        // （bit0を0遅延, bit1を120遅延）の直列で、全ビット一定遅延120の恒等。
        let n = QPSK_MAX_DELAY + 60;
        let mut tx_lsb: VecDeque<f32> = VecDeque::from(vec![0.0; QPSK_MAX_DELAY]); // bit0を120遅延
        let mut rx = BitDeinterleaverQpsk::new();

        let val = |t: usize, b: usize| (t * 10 + b + 1) as f32;
        let mut inputs: Vec<[f32; 2]> = Vec::new();
        for t in 0..n {
            let sym = [val(t, 0), val(t, 1)];
            inputs.push(sym);
            // TX interleave: bit0(lsb)を120遅延、bit1(msb)はそのまま
            tx_lsb.push_front(sym[0]);
            let tx_out = [*tx_lsb.back().unwrap(), sym[1]];
            tx_lsb.pop_back();
            // RX deinterleave
            let out = rx.push(tx_out);
            if t >= QPSK_MAX_DELAY {
                assert_eq!(out, inputs[t - QPSK_MAX_DELAY], "t={t}: 恒等が崩れた");
            }
        }
    }
}
