//! ③ チャネル等化：スキャッタードパイロット(SP)によるチャネル推定と等化。
//!
//! 流れ（gr-isdbt `ofdm_synchronization_impl.cc` の RX 側に倣う）：
//! 1. SPキャリアで `H[l] = Y[l] / pilot[l]`（既知パイロットで割る）
//! 2. SP間（12本間隔）を周波数方向に線形補間
//! 3. 両端は最寄りSP対の傾きで外挿
//! 4. 全キャリアを `X[l] = Y[l] / H[l]` で等化
//!
//! SPの位相（`symbol % 4`）は [`detect_symbol_phase`] が、隣接SPチャネル推定の
//! コヒーレンスを最大化する候補として求める（PRBS符号が正しいと隣接SP同士が
//! 揃い、コヒーレンス≈1になる性質を使う）。

use crate::params::FFT_LEN;
use crate::pilots::{SegmentPilots, SEGMENT_CARRIERS};
use num_complex::Complex32;

/// fftshift済み1024スペクトルの中で、中央セグメント432本が始まるbin。
/// `(1024 - 432)/2 = 296`（DCはbin512＝ローカルキャリア216）。
pub const SEGMENT_BIN_OFFSET: usize = (FFT_LEN - SEGMENT_CARRIERS) / 2;

/// fftshift済みスペクトル（長さ`FFT_LEN`）から中央セグメント432本を取り出す。
///
/// `bin_offset` は通常 [`SEGMENT_BIN_OFFSET`]。DCオフセットや半キャリアずれを
/// 経験的に詰めたいとき用に可変にしてある。
pub fn extract_segment(spectrum: &[Complex32], bin_offset: usize) -> Vec<Complex32> {
    spectrum[bin_offset..bin_offset + SEGMENT_CARRIERS].to_vec()
}

/// SPから周波数方向チャネル `H`（長さ432）を推定する。
///
/// `seg` は中央セグメント432本、`sym_mod4` はそのシンボルの `symbol%4`。
pub fn estimate_channel(
    seg: &[Complex32],
    sym_mod4: usize,
    pilots: &SegmentPilots,
) -> Vec<Complex32> {
    let n = SEGMENT_CARRIERS;
    let mut h = vec![Complex32::new(0.0, 0.0); n];
    let sp: Vec<usize> = pilots.sp_carriers(sym_mod4).collect();

    // 1) SP位置で生チャネル推定（パイロットは実数 ±4/3）
    for &l in &sp {
        h[l] = seg[l] / pilots.values[l];
    }

    // 2) 隣接SP間（12本間隔）を線形補間
    for w in sp.windows(2) {
        let (a, b) = (w[0], w[1]);
        let span = (b - a) as f32;
        let (ha, hb) = (h[a], h[b]);
        for l in (a + 1)..b {
            let t = (l - a) as f32 / span;
            h[l] = ha * (1.0 - t) + hb * t;
        }
    }

    // 3) 両端を傾きで外挿
    let first = sp[0];
    if first > 0 && sp.len() >= 2 {
        let slope = (h[sp[1]] - h[sp[0]]) / Complex32::new((sp[1] - sp[0]) as f32, 0.0);
        for l in 0..first {
            let d = l as f32 - first as f32;
            h[l] = h[first] + slope * Complex32::new(d, 0.0);
        }
    }
    let last = *sp.last().unwrap();
    if last < n - 1 && sp.len() >= 2 {
        let m = sp.len();
        let slope =
            (h[sp[m - 1]] - h[sp[m - 2]]) / Complex32::new((sp[m - 1] - sp[m - 2]) as f32, 0.0);
        for l in (last + 1)..n {
            let d = l as f32 - last as f32;
            h[l] = h[last] + slope * Complex32::new(d, 0.0);
        }
    }

    h
}

/// 推定チャネル `h` でセグメントを等化（`X = Y / H`）。
/// `|H|` が極小のbinは0にする（端の外挿が破綻した場合の保険）。
pub fn equalize(seg: &[Complex32], h: &[Complex32]) -> Vec<Complex32> {
    seg.iter()
        .zip(h)
        .map(|(&y, &hh)| {
            if hh.norm_sqr() < 1e-12 {
                Complex32::new(0.0, 0.0)
            } else {
                y / hh
            }
        })
        .collect()
}

/// 隣接SPチャネル推定のコヒーレンス（0..1）。
///
/// `|Σ H[i+1]·conj(H[i])| / Σ |H[i+1]||H[i]|`。チャネルが周波数方向に滑らか
/// かつ PRBS符号（＝`sym_mod4`）が正しいと隣接SPが揃い1に近づく。位相が違うと
/// SP位置・符号がずれてランダム化し小さくなる。
pub fn sp_coherence(seg: &[Complex32], sym_mod4: usize, pilots: &SegmentPilots) -> f32 {
    let mut num = Complex32::new(0.0, 0.0);
    let mut den = 0.0f32;
    let mut prev: Option<Complex32> = None;
    for l in pilots.sp_carriers(sym_mod4) {
        let h = seg[l] / pilots.values[l];
        if let Some(p) = prev {
            num += h * p.conj();
            den += h.norm() * p.norm();
        }
        prev = Some(h);
    }
    if den < 1e-12 {
        0.0
    } else {
        num.norm() / den
    }
}

/// SPコヒーレンス最大の `symbol%4` を返す（値とコヒーレンス）。
pub fn detect_symbol_phase(seg: &[Complex32], pilots: &SegmentPilots) -> (usize, f32) {
    (0..4)
        .map(|p| (p, sp_coherence(seg, p, pilots)))
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pilots::SegmentPilots;

    #[test]
    fn segment_bin_offset_is_296() {
        assert_eq!(SEGMENT_BIN_OFFSET, 296);
    }

    #[test]
    fn flat_channel_recovers_pilots() {
        // 平坦チャネル(=1)で、SP位置に正しいパイロット値を置けば H≈1、等化後も一致
        let pilots = SegmentPilots::center_1seg();
        let mut seg = vec![Complex32::new(0.3, -0.2); SEGMENT_CARRIERS]; // ダミーデータ
        for l in pilots.sp_carriers(0) {
            seg[l] = Complex32::new(pilots.values[l], 0.0); // チャネル1のSP
        }
        let h = estimate_channel(&seg, 0, &pilots);
        // SP位置のHは厳密に1
        for l in pilots.sp_carriers(0) {
            assert!(
                (h[l] - Complex32::new(1.0, 0.0)).norm() < 1e-5,
                "H[{l}] != 1"
            );
        }
        // コヒーレンスはほぼ1
        assert!(sp_coherence(&seg, 0, &pilots) > 0.999);
    }
}
