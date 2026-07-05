//! 実IQで ④→⑤ の全チェーンを通すプローブ（配管確認）。
//!
//! ②同期 → 復調 → ③等化 → データ抽出 → 周波数デインタ → 時間デインタ →
//! QPSKソフトデマップ → ビットデインタ → depuncture(2/3) → Viterbi。
//!
//! 注意：符号ビットの並び・パンクチャ位相・Viterbi始端には曖昧さがあり、
//! 「正しいTSが出るか」は ⑥（RS + 同期バイト0x47）でしか確定しない。本プローブは
//! チェーンが完走して妥当なビット列を生むことの確認まで。
//!
//! ```bash
//! cargo run -p isdbt-dsp --example viterbi_chain -- cap.iq 1015873 [nsym]
//! ```

use isdbt_dsp::deinterleave::{data_carrier_indices, freq_deinterleave, TimeDeinterleaver};
use isdbt_dsp::demap::{qpsk_soft, BitDeinterleaverQpsk};
use isdbt_dsp::demod::OfdmDemod;
use isdbt_dsp::equalize::{
    detect_symbol_phase, equalize, estimate_channel, extract_segment, SEGMENT_BIN_OFFSET,
};
use isdbt_dsp::iq::u8_iq_to_complex;
use isdbt_dsp::params::FFT_LEN;
use isdbt_dsp::pilots::SegmentPilots;
use isdbt_dsp::sync::estimate_sync;
use isdbt_dsp::viterbi::{depuncture, Viterbi, PUNCTURE_2_3};
use num_complex::Complex32;
use std::{env, fs};

fn main() {
    let mut a = env::args().skip(1);
    let path = a
        .next()
        .expect("usage: viterbi_chain <cap.iq> <fs_hz> [nsym]");
    let _fs: f64 = a.next().expect("fs_hz").parse().expect("fs数値");
    let nsym: usize = a.next().map(|s| s.parse().unwrap()).unwrap_or(1200);
    let il = 4usize; // TMCC: Layer A は I=4

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
    let mut bdi = BitDeinterleaverQpsk::new();
    let warmup = tdi.latency() + bdi.latency(); // 380 + 120

    // 全チェーンを流して符号ビットのソフト列を作る（warmup後のみ採用）
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
        let td = tdi.push_symbol(&fd);
        for v in td {
            let de = bdi.push(qpsk_soft(v)); // [lsb, msb]
            if k >= warmup {
                // 符号ビット順は [msb, lsb] と仮置き（⑥で確定）
                coded.push(de[1]);
                coded.push(de[0]);
            }
        }
    }

    eprintln!(
        "② start={} / 復調{}シンボル / phase0={phase0} / warmup={warmup}シンボル",
        est.symbol_start,
        specs.len()
    );
    eprintln!(
        "warmup後の符号ソフトビット数（パンクチャ済み）: {}",
        coded.len()
    );

    // depuncture(2/3) → Viterbi（メモリのためステップ数を上限）
    let restored = depuncture(&coded, &PUNCTURE_2_3);
    let max_steps = 60_000usize;
    let take = (max_steps * 2).min(restored.len() & !1);
    let v = Viterbi::new();
    let bits = v.decode(&restored[..take]);

    let ones = bits.iter().filter(|&&b| b == 1).count();
    println!("\n=== ④→⑤ チェーン（配管確認）===");
    println!("母符号ソフトビット(depuncture後) : {}", restored.len());
    println!("Viterbi復号ステップ              : {}", bits.len());
    println!(
        "復号ビットの1の割合              : {:.1}% (理想ペイロードは≈50%)",
        100.0 * ones as f32 / bits.len().max(1) as f32
    );
    eprintln!(
        "注：これは配管確認。正しいTSが出るかは ⑥（RS(204,188) + 同期バイト0x47探索で\n   ビット順・パンクチャ位相を確定）で判定する。現状C/Nだと弱い可能性も高い。"
    );
}
