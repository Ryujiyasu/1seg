//! 実IQで ④ デインターリーブのパイプラインを通すサニティ診断。
//!
//! ②同期 → 復調 → ③等化 → データキャリア抽出(384) → 周波数デインターリーブ →
//! 時間デインターリーブ(I=4) まで流し、出力が依然QPSK分布であること（＝置換・遅延の
//! 配管が壊れていない）と、レイテンシ後に正しく384キャリア/シンボル出ることを確認する。
//!
//! ```bash
//! cargo run -p isdbt-dsp --example deinterleave_probe -- cap.iq 1015873 [nsym] [I]
//! ```

use isdbt_dsp::deinterleave::{data_carrier_indices, freq_deinterleave, TimeDeinterleaver};
use isdbt_dsp::demod::OfdmDemod;
use isdbt_dsp::equalize::{
    detect_symbol_phase, equalize, estimate_channel, extract_segment, SEGMENT_BIN_OFFSET,
};
use isdbt_dsp::iq::u8_iq_to_complex;
use isdbt_dsp::params::FFT_LEN;
use isdbt_dsp::pilots::SegmentPilots;
use isdbt_dsp::sync::estimate_sync;
use num_complex::Complex32;
use std::{env, fs};

fn main() {
    let mut a = env::args().skip(1);
    let path = a
        .next()
        .expect("usage: deinterleave_probe <cap.iq> <fs_hz> [nsym] [I]");
    let _fs: f64 = a.next().expect("fs_hz").parse().expect("fs数値");
    let nsym: usize = a.next().map(|s| s.parse().unwrap()).unwrap_or(1200);
    let il: usize = a.next().map(|s| s.parse().unwrap()).unwrap_or(4); // TMCCより Layer A は I=4

    let bytes = fs::read(&path).expect("IQ読み込み");
    let mut s = u8_iq_to_complex(&bytes);
    let mean: Complex32 = s.iter().sum::<Complex32>() / s.len() as f32;
    for v in s.iter_mut() {
        *v -= mean;
    }
    let off = (s.len() / 10).min(50_000);
    let seg_sig = &s[off..];

    let est = estimate_sync(seg_sig, FFT_LEN).expect("同期できない");
    let demod = OfdmDemod::new(FFT_LEN);
    let specs = demod.demod_stream(
        seg_sig,
        est.symbol_start,
        est.guard,
        est.cfo_subcarriers,
        nsym,
    );

    let pilots = SegmentPilots::center_1seg();
    let seg0 = extract_segment(&specs[0], SEGMENT_BIN_OFFSET);
    let (phase0, _) = detect_symbol_phase(&seg0, &pilots);

    let mut tdi = TimeDeinterleaver::new(il);
    let latency = tdi.latency();
    eprintln!(
        "② start={} gi={:?} / 復調{}シンボル / phase0={phase0} / I={il} (時間デインタのレイテンシ={latency}シンボル)",
        est.symbol_start,
        est.guard,
        specs.len()
    );

    let mut out_points: Vec<Complex32> = Vec::new();
    for (k, sp) in specs.iter().enumerate() {
        let sym_mod4 = (phase0 + k) % 4;
        let seg = extract_segment(sp, SEGMENT_BIN_OFFSET);
        // ③ 等化
        let h = estimate_channel(&seg, sym_mod4, &pilots);
        let eq = equalize(&seg, &h);
        // データキャリア抽出 → 周波数デインタ → 時間デインタ
        let data: Vec<Complex32> = data_carrier_indices(sym_mod4, &pilots)
            .into_iter()
            .map(|l| eq[l])
            .collect();
        let fd = freq_deinterleave(&data);
        let td = tdi.push_symbol(&fd);
        if k >= latency {
            out_points.extend_from_slice(&td);
        }
    }

    eprintln!(
        "デインターリーブ後 有効シンボル数 {} / 出力データ点 {}",
        specs.len().saturating_sub(latency),
        out_points.len()
    );

    // サニティ：依然QPSK（4象限ほぼ均衡）か
    let mut q = [0usize; 4];
    for c in &out_points {
        let idx = (usize::from(c.re < 0.0)) | (usize::from(c.im < 0.0) << 1);
        q[idx] += 1;
    }
    let n = out_points.len().max(1) as f32;
    println!("\n=== ④ デインターリーブ後サニティ ===");
    println!(
        "象限分布(++,-+,+-,--) : {} {} {} {}  (各≈{:.1}%)",
        q[0], q[1], q[2], q[3], 25.0
    );
    let bal = q
        .iter()
        .map(|&x| (x as f32 / n - 0.25).abs())
        .fold(0.0, f32::max);
    println!(
        "象限の最大偏り        : {:.1}%  → {}",
        bal * 100.0,
        if bal < 0.05 {
            "✅ QPSK構造を保持（配管OK）"
        } else {
            "△ 偏りあり（C/N or 位相）"
        }
    );
    eprintln!("注：デインターリーブは置換＋遅延なのでコンステ自体は不変。これは配管の健全性チェック。次は ④デマップ→⑤Viterbi。");
}
