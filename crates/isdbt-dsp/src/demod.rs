//! ②→③ の橋渡し：OFDM復調（小数CFO補正 → CPスキップ → FFT → fftshift）。
//!
//! 同期 [`crate::sync`] で得た `symbol_start` / `guard` / `cfo` を使い、
//! 各OFDMシンボルから `fft_len` 本の副搬送波（複素）を取り出す。
//! 出力は fftshift 済み（DC＝carrier0 が中央）。中央付近の約432本が
//! 1セグメントのキャリア。チャネル等化はこの後段（③）。

use crate::params::GuardInterval;
use num_complex::Complex32;
use rustfft::{Fft, FftPlanner};
use std::f32::consts::PI;
use std::sync::Arc;

/// 固定FFT長のOFDM復調器（プランを使い回す）。
pub struct OfdmDemod {
    fft: Arc<dyn Fft<f32>>,
    n: usize,
}

impl OfdmDemod {
    pub fn new(fft_len: usize) -> Self {
        let mut planner = FftPlanner::<f32>::new();
        Self {
            fft: planner.plan_fft_forward(fft_len),
            n: fft_len,
        }
    }

    pub fn fft_len(&self) -> usize {
        self.n
    }

    /// `symbol_start` 以降の各シンボルを復調し、副搬送波スペクトル列を返す。
    ///
    /// - `cfo_subcarriers`：[`crate::sync`] の小数CFO（±0.5）。時間領域で
    ///   `exp(-j2π ε m / N)`（m は絶対サンプルindex）として連続補正する。
    /// - CPは捨て、有効シンボル `N` 点だけをFFTする。
    /// - 返り値の各 `Vec` は長さ `N`、**fftshift済み**（index N/2 が DC）。
    pub fn demod_stream(
        &self,
        r: &[Complex32],
        symbol_start: usize,
        gi: GuardInterval,
        cfo_subcarriers: f32,
        max_symbols: usize,
    ) -> Vec<Vec<Complex32>> {
        let n = self.n;
        let l = gi.cp_len(n);
        let sym = n + l;
        let half = n / 2;
        let scale = 1.0 / (n as f32).sqrt();

        let mut out = Vec::new();
        let mut buf = vec![Complex32::new(0.0, 0.0); n];
        let mut s = symbol_start;
        while out.len() < max_symbols && s + sym <= r.len() {
            let base = s + l; // CPをスキップした有効シンボル先頭
            for k in 0..n {
                let m = base + k;
                let ph = -2.0 * PI * cfo_subcarriers * (m as f32) / (n as f32);
                buf[k] = r[m] * Complex32::from_polar(1.0, ph);
            }
            self.fft.process(&mut buf);

            // fftshift（DCを中央 index N/2 へ）＋ 正規化
            let mut spec = vec![Complex32::new(0.0, 0.0); n];
            for k in 0..n {
                spec[(k + half) % n] = buf[k] * scale;
            }
            out.push(spec);
            s += sym;
        }
        out
    }

    /// 1シンボルだけ復調（ストリーミング/逐次処理用）。`r[sym_start ..]` から
    /// CPを飛ばして有効N点をFFT・fftshift。CFO位相は**シンボルローカル**（m=0..N）で、
    /// シンボル間の定数位相差は後段の[`crate::equalize`]（SPで毎シンボルH推定）が吸収する。
    /// 返り値は長さ`fft_len`のfftshift済みスペクトル。
    pub fn demod_one(
        &self,
        r: &[Complex32],
        sym_start: usize,
        gi: GuardInterval,
        cfo_subcarriers: f32,
    ) -> Vec<Complex32> {
        let n = self.n;
        let l = gi.cp_len(n);
        let half = n / 2;
        let scale = 1.0 / (n as f32).sqrt();
        let base = sym_start + l;
        let mut buf = vec![Complex32::new(0.0, 0.0); n];
        // CFO補正：sin/cosを毎サンプル呼ばず、定数位相子を掛け続ける増分回転（大幅高速化）。
        let w = Complex32::from_polar(1.0, -2.0 * PI * cfo_subcarriers / (n as f32));
        let mut ph = Complex32::new(1.0, 0.0);
        for k in 0..n {
            buf[k] = r[base + k] * ph;
            ph *= w;
        }
        self.fft.process(&mut buf);
        let mut spec = vec![Complex32::new(0.0, 0.0); n];
        for k in 0..n {
            spec[(k + half) % n] = buf[k] * scale;
        }
        spec
    }
}
