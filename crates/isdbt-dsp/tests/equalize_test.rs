//! ③ チャネル等化の単体検証。
//!
//! 仕様どおりのSP（PRBS値）＋QPSKデータで1セグメントを組み、既知の
//! 「周波数方向に線形」なチャネルを掛ける。線形チャネルなら SP 間の線形補間と
//! 端の傾き外挿で厳密に推定できるはずなので、等化後にデータが復元されること、
//! および `symbol%4` 位相が正しく検出されることを確認する。

use isdbt_dsp::equalize::{detect_symbol_phase, equalize, estimate_channel, sp_coherence};
use isdbt_dsp::pilots::{SegmentPilots, SEGMENT_CARRIERS};
use num_complex::Complex32;
use std::collections::HashSet;

fn qpsk(idx: usize) -> Complex32 {
    let s = 1.0 / 2f32.sqrt();
    let re = if idx & 1 == 0 { s } else { -s };
    let im = if idx & 2 == 0 { s } else { -s };
    Complex32::new(re, im)
}

/// 周波数方向に線形なチャネル H[l] = (1+0.3t) + j(-0.2+0.4t), t=l/431。
fn linear_channel(n: usize) -> Vec<Complex32> {
    (0..n)
        .map(|l| {
            let t = l as f32 / (n as f32 - 1.0);
            Complex32::new(1.0 + 0.3 * t, -0.2 + 0.4 * t)
        })
        .collect()
}

#[test]
fn equalizes_linear_channel_and_detects_phase() {
    let pilots = SegmentPilots::center_1seg();
    let n = SEGMENT_CARRIERS;
    let sym = 2usize; // テスト対象の symbol%4

    let sp: HashSet<usize> = pilots.sp_carriers(sym).collect();

    // 送信セグメント X：SPは仕様値、それ以外はQPSK
    let mut x = vec![Complex32::new(0.0, 0.0); n];
    for l in 0..n {
        x[l] = if sp.contains(&l) {
            Complex32::new(pilots.values[l], 0.0)
        } else {
            qpsk(l)
        };
    }

    // 受信 Y = X·H
    let h_true = linear_channel(n);
    let y: Vec<Complex32> = x.iter().zip(&h_true).map(|(a, b)| a * b).collect();

    // 位相検出：正しい symbol%4 を当て、コヒーレンスはほぼ1
    let (phase, coh) = detect_symbol_phase(&y, &pilots);
    assert_eq!(phase, sym, "symbol%4の検出ミス");
    assert!(coh > 0.99, "コヒーレンスが低い: {coh}");
    // 誤った位相はコヒーレンスが明確に低いこと
    for wrong in (0..4).filter(|&p| p != sym) {
        assert!(
            sp_coherence(&y, wrong, &pilots) < 0.9,
            "誤位相{wrong}のコヒーレンスが高すぎる"
        );
    }

    // 推定→等化：線形チャネルなので厳密復元できるはず
    let h = estimate_channel(&y, sym, &pilots);
    let xeq = equalize(&y, &h);

    let mut worst = 0.0f32;
    for l in 0..n {
        if !sp.contains(&l) {
            worst = worst.max((xeq[l] - x[l]).norm());
        }
    }
    assert!(worst < 1e-3, "データ復元誤差が大きい worst={worst}");
}
