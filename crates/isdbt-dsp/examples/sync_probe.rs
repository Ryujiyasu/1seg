//! 実IQ（rtl_sdr の u8 raw）を読んで ② OFDM同期が掛かるかを確認する診断ツール。
//!
//! ```bash
//! rtl_sdr -f 497142857 -s 1015873 -g 49 -n 10158730 cap.iq   # ch17 を ~10s
//! cargo run -p isdbt-dsp --example sync_probe -- cap.iq 1015873
//! ```
//! 第2引数を省くと既定サンプルレート（1seg Mode3 ≈ 1.016MHz）を使う。

use isdbt_dsp::iq::u8_iq_to_complex;
use isdbt_dsp::params::{GuardInterval, FFT_LEN, SAMPLE_RATE_HZ};
use isdbt_dsp::sync::{estimate_sync, metric_curve, periodicity_score};
use num_complex::Complex32;
use std::{env, fs};

fn main() {
    let mut args = env::args().skip(1);
    let path = match args.next() {
        Some(p) => p,
        None => {
            eprintln!("usage: sync_probe <capture.iq> [fs_hz]");
            std::process::exit(2);
        }
    };
    let fs: f64 = args
        .next()
        .map(|s| s.parse().expect("fs_hz は数値"))
        .unwrap_or(SAMPLE_RATE_HZ);

    let bytes = fs::read(&path).expect("IQファイル読み込み");
    let mut s = u8_iq_to_complex(&bytes);
    println!(
        "読み込み: {} サンプル ({:.2}s @ {:.0} Hz)",
        s.len(),
        s.len() as f64 / fs,
        fs
    );
    if s.len() < FFT_LEN * 4 {
        eprintln!("サンプルが少なすぎる");
        std::process::exit(1);
    }

    // DC除去（RTLのDCスパイク対策）と平均電力
    let mean: Complex32 = s.iter().sum::<Complex32>() / s.len() as f32;
    for v in s.iter_mut() {
        *v -= mean;
    }
    let pwr: f32 = s.iter().map(|v| v.norm_sqr()).sum::<f32>() / s.len() as f32;
    println!(
        "DC = {:.4}{:+.4}j を除去 / 平均電力 {:.5} ({:.1} dBFS)",
        mean.re,
        mean.im,
        pwr,
        10.0 * pwr.log10()
    );

    // 解析は先頭の一部に絞る（処理時間バウンド）
    let win = s.len().min(300_000);
    let seg = &s[..win];

    // GI判定：4候補の周期性スコア（折り畳みくしの高さ）を比較
    println!("\n=== GI周期性スコア（高いほど本物）===");
    for &gi in &GuardInterval::ALL {
        let sc = periodicity_score(seg, FFT_LEN, gi);
        let bar = "#".repeat((sc * 40.0) as usize);
        println!("  {:?}\t(sym={:>4}) score={:.3} {}", gi, gi.symbol_len(FFT_LEN), sc, bar);
    }

    let best = estimate_sync(seg, FFT_LEN).expect("十分なサンプル");
    let carrier_spacing = fs / FFT_LEN as f64;
    println!("\n=== ② OFDM同期 結果 ===");
    println!("GI推定        : {:?}", best.guard);
    println!("symbol_start  : {}", best.symbol_start);
    println!("metric(0..1)  : {:.3}", best.metric);
    println!(
        "小数CFO       : {:.4} 副搬送波 = {:+.1} Hz",
        best.cfo_subcarriers,
        best.cfo_subcarriers as f64 * carrier_spacing
    );

    // 周期性の確認：検出GIで metric_curve を作り、N+L ごとにピークが立つか
    let sym = best.guard.symbol_len(FFT_LEN);
    let curve = metric_curve(seg, FFT_LEN, best.guard);
    println!("\n周期性チェック（sym長 = {}）:", sym);
    for k in -2..=2i64 {
        let idx = best.symbol_start as i64 + k * sym as i64;
        if idx >= 0 && (idx as usize) < curve.len() {
            let bar = "#".repeat((curve[idx as usize] * 40.0) as usize);
            println!("  d={:>7} (#{:+}) M={:.3} {}", idx, k, curve[idx as usize], bar);
        }
    }

    let mut sorted = curve.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let med = sorted[sorted.len() / 2];
    let max = *sorted.last().unwrap();
    let ratio = max / med.max(1e-6);
    println!("\nメトリクス: max={:.3}, median={:.3} → ピーク/床比 {:.1}x", max, med, ratio);

    if best.metric > 0.5 && ratio > 2.0 {
        println!("\n✅ CP相関ロック：実信号でOFDM同期が掛かっている可能性が高い");
    } else {
        println!("\n⚠ ロック弱い：信号が弱い / 周波数ずれ / アンテナ未接続の可能性");
    }
}
