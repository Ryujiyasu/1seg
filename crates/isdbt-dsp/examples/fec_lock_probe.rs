//! ⑥に入る前の関門：FEC（畳み込み符号）が実機でロックするかを探索する。
//!
//! 符号ビットの並び・全体シフト・パンクチャ位相には曖昧さがある。これらを総当たりし、
//! 各設定で Viterbi 復号 → 再エンコード → 受信ハードビットとの一致率（≈ 1−BER）を測る。
//! **正しい設定なら一致率がスパイク**するはず（③のbin探索・④のTMCC同期と同じ発想）。
//! どれもスパイクしなければ、それは C/N の壁（弱アンテナ）の証拠。
//!
//! ```bash
//! cargo run -p isdbt-dsp --example fec_lock_probe -- cap.iq 1015873 [nsym]
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
use isdbt_dsp::viterbi::{conv_encode, depuncture, puncture, Viterbi, PUNCTURE_2_3};
use num_complex::Complex32;
use std::{env, fs};

fn main() {
    let mut a = env::args().skip(1);
    let path = a
        .next()
        .expect("usage: fec_lock_probe <cap.iq> <fs_hz> [nsym]");
    let _fs: f64 = a.next().expect("fs_hz").parse().expect("fs数値");
    let nsym: usize = a.next().map(|s| s.parse().unwrap()).unwrap_or(1600);
    let il = 4usize;

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

    // ④ チェーンを流して、warmup後の per-carrier soft [lsb, msb] を集める
    let mut tdi = TimeDeinterleaver::new(il);
    let mut bdi = BitDeinterleaverQpsk::new();
    let warmup = tdi.latency() + bdi.latency();
    let mut carriers: Vec<[f32; 2]> = Vec::new();
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
        for v in tdi.push_symbol(&fd) {
            let de = bdi.push(qpsk_soft(v));
            if k >= warmup {
                carriers.push(de);
            }
        }
    }
    eprintln!(
        "② start={} / 復調{}シンボル / phase0={phase0} / warmup後キャリア {}",
        est.symbol_start,
        specs.len(),
        carriers.len()
    );

    let v = Viterbi::new();
    let skip = 400usize; // Viterbi始端の不確かさを捨てる（パンクチャ済みビット単位）
    let max_steps = 40_000usize;

    println!("\n=== FECロック探索（一致率 = 再エンコードと受信の符号ビット一致, ≈1−BER）===");
    println!("order shift pphase :  match");
    let mut best = (0.0f32, (0usize, 0usize, 0usize));
    for order in 0..2 {
        // 0: [lsb,msb] の順で出す, 1: [msb,lsb]
        for shift in 0..2 {
            // 符号ストリーム全体の偶奇シフト
            // パンクチャ済み符号ソフト列を構築
            let mut coded: Vec<f32> = Vec::with_capacity(carriers.len() * 2);
            for c in &carriers {
                if order == 0 {
                    coded.push(c[0]);
                    coded.push(c[1]);
                } else {
                    coded.push(c[1]);
                    coded.push(c[0]);
                }
            }
            let coded = &coded[shift..];
            for pphase in 0..3 {
                // パンクチャ位相：先頭から pphase 個の kept ビットを捨てて周期境界を合わせる
                let punct = &coded[pphase..];
                let restored = depuncture(punct, &PUNCTURE_2_3);
                let take = (max_steps * 2).min(restored.len() & !1);
                if take < 2000 {
                    continue;
                }
                let info = v.decode(&restored[..take]);
                // 再エンコード → パンクチャ → 受信ハードと照合
                let (mother, _) = conv_encode(&info, 0);
                let reenc = puncture(&mother, &PUNCTURE_2_3);
                // 受信パンクチャ済みハード（restored の非消失位置 = punct の並び）
                let n = reenc.len().min(punct.len());
                let mut agree = 0usize;
                let mut total = 0usize;
                for i in skip..n {
                    let rx = u8::from(punct[i] > 0.0);
                    if reenc[i] == rx {
                        agree += 1;
                    }
                    total += 1;
                }
                let m = if total > 0 {
                    agree as f32 / total as f32
                } else {
                    0.0
                };
                println!("  {order}     {shift}     {pphase}    : {:.3}", m);
                if m > best.0 {
                    best = (m, (order, shift, pphase));
                }
            }
        }
    }

    // 較正：純雑音(±1ランダム)で同じ指標を測る。これがこの指標の「床」。
    // 実信号がこの床と同じなら、畳み込み構造は出ていない＝C/Nの壁。
    let mut seed = 0x1234_5678u32;
    let mut rng = || {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        if (seed >> 16) & 1 == 1 {
            1.0f32
        } else {
            -1.0f32
        }
    };
    let nnoise = (max_steps * 3).min(carriers.len() * 2);
    let noise: Vec<f32> = (0..nnoise).map(|_| rng()).collect();
    let restored = depuncture(&noise, &PUNCTURE_2_3);
    let take = restored.len() & !1;
    let info = v.decode(&restored[..take]);
    let (mother, _) = conv_encode(&info, 0);
    let reenc = puncture(&mother, &PUNCTURE_2_3);
    let n = reenc.len().min(noise.len());
    let mut agree = 0usize;
    for i in skip..n {
        if reenc[i] == u8::from(noise[i] > 0.0) {
            agree += 1;
        }
    }
    let noise_floor = agree as f32 / (n - skip) as f32;

    let (bm, (bo, bs, bp)) = best;
    println!("\n純雑音での同指標（床）: {:.3}", noise_floor);
    println!("最良: order={bo} shift={bs} pphase={bp}  一致率 {:.5}", bm);
    println!(
        "実信号 − 雑音床 = {:+.3}  → {}",
        bm - noise_floor,
        if bm - noise_floor > 0.03 {
            "床より有意に高い（弱いが構造あり）"
        } else {
            "床とほぼ同じ（＝畳み込み構造が出ていない）"
        }
    );
    // 正しい判定は「雑音床を有意に超えるか」。再エンコード一致率は単独では
    // 雑音でも高く出る（Viterbiが常に最良経路を選ぶ）ため、床との差で見る。
    println!(
        "判定: {}",
        if bm - noise_floor > 0.06 {
            "✅ FECロック（雑音床を明確に超過 → ⑥ RS/逆拡散へ進める）"
        } else if bm - noise_floor > 0.02 {
            "△ 弱い構造あり（C/Nぎりぎり。RSは厳しいかも）"
        } else {
            "✗ ロックせず（雑音床と同等以下＝畳み込み構造が出ていない＝C/Nの壁。録り直し推奨）"
        }
    );
    eprintln!("注：再エンコード一致率は雑音でも高く出るので、判定は『雑音床との差』で行う。");
}
