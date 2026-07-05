//! ② OFDM同期：ガードインターバル（CP）の自己相関による
//! シンボルタイミング推定 と 小数キャリア周波数オフセット（CFO）推定。
//!
//! CP は有効シンボルの末尾 L サンプルのコピーなので、
//! `r[j]` と `r[j+N]` は CP 区間で強く相関する。これを使う
//! van de Beek ら (1997) の最尤（ML）推定。
//!
//! 各開始位置 d について
//! ```text
//!   γ(d) = Σ_{k=0..L-1}  r[d+k]·conj(r[d+k+N])
//!   Φ(d) = Σ_{k=0..L-1}  (|r[d+k]|² + |r[d+k+N]|²) / 2
//!   M(d) = |γ(d)| / Φ(d)            （0..1、1で完全相関）
//! ```
//! を計算し、M(d) が最大になる d をシンボル境界とする。
//! そのときの位相から小数CFO（キャリア間隔単位、±0.5）が
//! `ε = -arg(γ) / 2π` で求まる。

use crate::params::GuardInterval;
use num_complex::Complex32;
use std::f32::consts::PI;

/// 同期推定の結果。
#[derive(Clone, Copy, Debug)]
pub struct SyncEstimate {
    /// 推定シンボル開始位置（CP先頭のサンプル index）。
    pub symbol_start: usize,
    /// タイミングメトリクスの尖り具合（0..1、1に近いほど確信が高い）。
    pub metric: f32,
    /// 小数CFO（キャリア間隔単位、±0.5）。整数CFOは後段（パイロット）で。
    pub cfo_subcarriers: f32,
    /// 仮定したガードインターバル。
    pub guard: GuardInterval,
}

impl SyncEstimate {
    /// 小数CFO を Hz へ。`carrier_spacing_hz` は [`crate::params::CARRIER_SPACING_HZ`]。
    pub fn cfo_hz(&self, carrier_spacing_hz: f64) -> f64 {
        self.cfo_subcarriers as f64 * carrier_spacing_hz
    }
}

/// CP自己相関メトリクスを全位置で計算し、最大の位置を返す。
///
/// `r` は入力IQ、`fft_len = N`、`gi` で CP長 L が決まる。
/// 安定した推定には最低でも数シンボル分（`>= 2*(N+L)`）あると良い。
/// サンプルが足りない（`< N+L`）場合は `None`。
pub fn estimate_symbol_sync(
    r: &[Complex32],
    fft_len: usize,
    gi: GuardInterval,
) -> Option<SyncEstimate> {
    let n = fft_len;
    let l = gi.cp_len(fft_len);
    if l == 0 || r.len() < n + l {
        return None;
    }
    // d は 0..=d_max（r[d+L-1+N] が読める最大の d）
    let d_max = r.len() - n - l;

    // term(j) = r[j]·conj(r[j+N]),  p(j) = (|r[j]|² + |r[j+N]|²)/2
    let term = |j: usize| r[j] * r[j + n].conj();
    let p = |j: usize| (r[j].norm_sqr() + r[j + n].norm_sqr()) * 0.5;

    // d=0 の窓を初期化
    let mut gamma = Complex32::new(0.0, 0.0);
    let mut phi = 0.0f32;
    for j in 0..l {
        gamma += term(j);
        phi += p(j);
    }

    let make = |d: usize, gamma: Complex32, phi: f32| SyncEstimate {
        symbol_start: d,
        metric: if phi > 0.0 { gamma.norm() / phi } else { 0.0 },
        cfo_subcarriers: -gamma.arg() / (2.0 * PI),
        guard: gi,
    };

    let mut best = make(0, gamma, phi);

    // スライディング窓で d=1..=d_max を更新（O(N) で全位置を走査）
    for d in 1..=d_max {
        gamma += term(d + l - 1) - term(d - 1);
        phi += p(d + l - 1) - p(d - 1);
        let cand = make(d, gamma, phi);
        if cand.metric > best.metric {
            best = cand;
        }
    }
    Some(best)
}

/// 全位置の正規化メトリクス `M(d)=|γ(d)|/Φ(d)` を返す（診断・可視化用）。
///
/// 長さは `r.len() - N - L + 1`。実信号でCP相関ピークが
/// シンボル周期 `N+L` ごとに立つかを確認するのに使う。
pub fn metric_curve(r: &[Complex32], fft_len: usize, gi: GuardInterval) -> Vec<f32> {
    let n = fft_len;
    let l = gi.cp_len(fft_len);
    if l == 0 || r.len() < n + l {
        return Vec::new();
    }
    let d_max = r.len() - n - l;
    let term = |j: usize| r[j] * r[j + n].conj();
    let p = |j: usize| (r[j].norm_sqr() + r[j + n].norm_sqr()) * 0.5;

    let mut gamma = Complex32::new(0.0, 0.0);
    let mut phi = 0.0f32;
    for j in 0..l {
        gamma += term(j);
        phi += p(j);
    }
    let mut out = Vec::with_capacity(d_max + 1);
    out.push(if phi > 0.0 { gamma.norm() / phi } else { 0.0 });
    for d in 1..=d_max {
        gamma += term(d + l - 1) - term(d - 1);
        phi += p(d + l - 1) - p(d - 1);
        out.push(if phi > 0.0 { gamma.norm() / phi } else { 0.0 });
    }
    out
}

/// メトリクス曲線 `curve` を周期 `period` で折り畳んだ位相平均を返す（長さ `period`）。
///
/// 本物のGIなら、真の境界位相で平均くしが高く尖る。偽のGIだと全位相で平坦。
pub fn fold_metric(curve: &[f32], period: usize) -> Vec<f32> {
    let mut fold = vec![0.0f32; period];
    let mut cnt = vec![0u32; period];
    for (i, &m) in curve.iter().enumerate() {
        fold[i % period] += m;
        cnt[i % period] += 1;
    }
    for j in 0..period {
        if cnt[j] > 0 {
            fold[j] /= cnt[j] as f32;
        }
    }
    fold
}

/// 周期性スコア：`gi` を仮定したときの折り畳み平均くしの最大高さ（0..1）。
/// 「CPが周期 N+L で繰り返し立っているか」を表す。GI判定の根拠。
pub fn periodicity_score(r: &[Complex32], fft_len: usize, gi: GuardInterval) -> f32 {
    let curve = metric_curve(r, fft_len, gi);
    let period = gi.symbol_len(fft_len);
    if curve.len() < 2 * period {
        return 0.0;
    }
    fold_metric(&curve, period).into_iter().fold(0.0, f32::max)
}

/// GIが未知のとき、**周期性**でGIを選んで同期する（正しい検出）。
///
/// 各GI候補のメトリクス曲線を `N+L` で折り畳み、平均くしが最も高い
/// （＝CPが周期的に立っている）GIを採用する。単発メトリクス最大だと
/// 最短GI(1/32)へ誤判定するため、通常はこちらを使う。
pub fn estimate_sync(r: &[Complex32], fft_len: usize) -> Option<SyncEstimate> {
    let gi = GuardInterval::ALL
        .iter()
        .copied()
        .map(|gi| (periodicity_score(r, fft_len, gi), gi))
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, gi)| gi)?;
    estimate_symbol_sync(r, fft_len, gi)
}

/// GIが未知のとき、4候補すべてを試して単発メトリクス最大のものを返す（**暫定・非推奨**）。
///
/// 注意：正規化メトリクスは「CPより短いL」でも 1 付近に張り付くため、
/// これだけでGIを確定すると最短GIへ誤判定する。GI判定には周期性を見る
/// [`estimate_sync`] を使うこと。
pub fn estimate_best_guard(r: &[Complex32], fft_len: usize) -> Option<SyncEstimate> {
    GuardInterval::ALL
        .iter()
        .filter_map(|&gi| estimate_symbol_sync(r, fft_len, gi))
        .max_by(|a, b| {
            a.metric
                .partial_cmp(&b.metric)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}
