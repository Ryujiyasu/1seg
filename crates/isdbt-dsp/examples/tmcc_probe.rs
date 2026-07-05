//! 実IQから ④の入口・TMCC を復号する診断ツール。
//!
//! ②同期→復調→中央セグメント抽出→TMCCキャリアのDBPSK→フレーム同期(204)→
//! 多数決でフレーム統合→伝送パラメータをパースして表示する。
//!
//! ```bash
//! cargo run -p isdbt-dsp --example tmcc_probe -- cap.iq 1015873 [nsym]
//! ```

use isdbt_dsp::demod::OfdmDemod;
use isdbt_dsp::equalize::{extract_segment, SEGMENT_BIN_OFFSET};
use isdbt_dsp::iq::u8_iq_to_complex;
use isdbt_dsp::params::FFT_LEN;
use isdbt_dsp::sync::estimate_sync;
use isdbt_dsp::tmcc::{
    coding_rate_str, dbpsk_bits, find_frame_sync, interleaving_mode3, majority_frame, parse_tmcc,
    LayerInfo, SYMBOLS_PER_FRAME,
};
use num_complex::Complex32;
use std::{env, fs};

fn layer_line(tag: &str, l: &LayerInfo) {
    let il = interleaving_mode3(l.interleaving)
        .map(|i| format!("I={i}"))
        .unwrap_or_else(|| "I=未使用".into());
    println!(
        "  Layer {tag}: 変調={:?}  符号化率={}  時間IL={}  セグ数={}",
        l.modulation,
        coding_rate_str(l.coding_rate),
        il,
        l.n_segments
    );
}

fn main() {
    let mut a = env::args().skip(1);
    let path = a.next().expect("usage: tmcc_probe <cap.iq> <fs_hz> [nsym]");
    let fs: f64 = a.next().expect("fs_hz").parse().expect("fs数値");
    let nsym: usize = a.next().map(|s| s.parse().unwrap()).unwrap_or(2000);

    let bytes = fs::read(&path).expect("IQ読み込み");
    let mut s = u8_iq_to_complex(&bytes);
    let mean: Complex32 = s.iter().sum::<Complex32>() / s.len() as f32;
    for v in s.iter_mut() {
        *v -= mean;
    }
    let off = (s.len() / 10).min(50_000);
    let seg_sig = &s[off..];

    let est = estimate_sync(seg_sig, FFT_LEN).expect("同期できない");
    eprintln!(
        "② sync: start={} gi={:?} metric={:.3} cfo={:.1}Hz",
        est.symbol_start,
        est.guard,
        est.metric,
        est.cfo_subcarriers as f64 * fs / FFT_LEN as f64
    );

    let demod = OfdmDemod::new(FFT_LEN);
    let specs = demod.demod_stream(
        seg_sig,
        est.symbol_start,
        est.guard,
        est.cfo_subcarriers,
        nsym,
    );
    let segs: Vec<Vec<Complex32>> = specs
        .iter()
        .map(|sp| extract_segment(sp, SEGMENT_BIN_OFFSET))
        .collect();
    eprintln!(
        "復調 {} シンボル（≈{:.1} フレーム）",
        segs.len(),
        segs.len() as f32 / SYMBOLS_PER_FRAME as f32
    );

    // TMCC：DBPSK復号 → フレーム同期
    let bits = dbpsk_bits(&segs);
    let fsync = find_frame_sync(&bits).expect("フレーム同期できるビット数がない");

    println!("\n=== ④ TMCC フレーム同期 ===");
    println!("フレーム位相(B0位置) : {}", fsync.phase);
    println!("評価フレーム数        : {}", fsync.n_frames);
    println!(
        "同期語一致            : {}/{} ({:.1}%)",
        fsync.matched,
        fsync.total,
        100.0 * fsync.matched as f32 / fsync.total as f32
    );
    let par: String = fsync
        .parity_per_frame
        .iter()
        .map(|&o| if o { 'O' } else { 'E' })
        .collect();
    println!(
        "各フレームの偶奇      : {par}  （交互={}）",
        fsync.alternates
    );
    println!(
        "判定                  : {}",
        if fsync.alternates && fsync.matched * 100 >= fsync.total * 90 {
            "✅ TMCCロック（同期語が偶奇交互＝フレーム整合の強い証拠）"
        } else {
            "△ 同期弱い（C/N不足の可能性）"
        }
    );

    // 情報部をフレーム間多数決で統合してパース
    let frame = majority_frame(&bits, fsync.phase);
    let info = parse_tmcc(&frame);

    println!("\n=== TMCC 伝送パラメータ ===");
    println!(
        "システム識別={} / 伝送切替指標={} / 緊急警報={} / 部分受信={}",
        info.system_id, info.switching_indicator, info.emergency_flag, info.partial_reception
    );
    layer_line("A(1seg)", &info.layer_a);
    layer_line("B", &info.layer_b);
    layer_line("C", &info.layer_c);
    eprintln!(
        "\n注：捕捉は中央1セグのみ。TMCCは全帯域共通なのでB/Cも読めるが、物理的に持つのはLayer A。"
    );
}
