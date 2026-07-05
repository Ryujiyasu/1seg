//! ⑥ 実電波から MPEG-TS の同期バイト 0x47/0xB8 を探す ── 「TSが出た」の瞬間。
//!
//! 確定したFEC設定(order=1,shift=0,pphase=0)でViterbi復号 → バイト化 →
//! Forneyバイトデインターリーブ → 0x47/0xB8 が204バイト周期で立つ位相を探索する。
//!
//! ```bash
//! cargo run -p isdbt-dsp --example ts_probe -- cap.iq 1015873 [nsym]
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
use isdbt_dsp::ts::{best_sync_phase, pack_bits_msb, ByteDeinterleaver, BI_I, BI_M, TSP};
use isdbt_dsp::viterbi::{depuncture, Viterbi, PUNCTURE_2_3};
use num_complex::Complex32;
use std::{env, fs};

fn main() {
    let mut a = env::args().skip(1);
    let path = a.next().expect("usage: ts_probe <cap.iq> <fs_hz> [nsym]");
    let _fs: f64 = a.next().expect("fs_hz").parse().expect("fs数値");
    let nsym: usize = a.next().map(|s| s.parse().unwrap()).unwrap_or(3000);
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

    // ④→⑤ 確定設定(order=1: [msb,lsb]) で符号ソフト列を作る
    let mut tdi = TimeDeinterleaver::new(il);
    let mut bdi = BitDeinterleaverQpsk::new();
    let warmup = tdi.latency() + bdi.latency();
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
        for v in tdi.push_symbol(&fd) {
            let de = bdi.push(qpsk_soft(v));
            if k >= warmup {
                coded.push(de[1]); // msb
                coded.push(de[0]); // lsb
            }
        }
    }

    // depuncture(2/3, pphase=0) → Viterbi → 情報ビット
    let restored = depuncture(&coded, &PUNCTURE_2_3);
    let max_steps = 300_000usize;
    let take = (max_steps * 2).min(restored.len() & !1);
    let bits = Viterbi::new().decode(&restored[..take]);
    eprintln!(
        "② start={} / 復調{}シンボル / Viterbi情報ビット {} (≈{}バイト)",
        est.symbol_start,
        specs.len(),
        bits.len(),
        bits.len() / 8
    );

    // バイト化(bit_offset) × コミュテータ位相(feed_off) を探索し、0x47/0xB8 の204周期性を測る
    let latency = BI_M * BI_I * (BI_I - 1); // 2244バイト
    let mut best = (0.0f32, 0usize, 0usize, 0usize); // score, bit_off, feed_off, phase
    for bit_off in 0..8 {
        let bytes = pack_bits_msb(&bits, bit_off);
        if bytes.len() < latency + 20 * TSP {
            continue;
        }
        for feed_off in 0..BI_I {
            let mut di = ByteDeinterleaver::new();
            let mut out = Vec::with_capacity(bytes.len());
            for (j, &b) in bytes[feed_off..].iter().enumerate() {
                let o = di.push(b);
                if j >= latency {
                    out.push(o);
                }
            }
            let (phase, score) = best_sync_phase(&out);
            if score > best.0 {
                best = (score, bit_off, feed_off, phase);
            }
        }
    }

    let (score, bit_off, feed_off, phase) = best;
    println!("\n=== ⑥ TS同期探索（0x47/0xB8 が204バイト周期で立つか）===");
    println!("最良: bit_offset={bit_off} commutator={feed_off} block_phase={phase}");
    println!("同期バイト命中率(204周期) : {:.1}%", score * 100.0);
    println!(
        "判定: {}",
        if score > 0.95 {
            "🎉 TSロック！ 自作復調器が実電波からMPEG-TSを取り出した"
        } else if score > 0.5 {
            "△ 部分的（整列は近いが誤りあり → RSで拾える可能性）"
        } else {
            "✗ 同期バイトが立たない（設定orバイトデインタ要見直し）"
        }
    );

    // 参考：最良整列で先頭数ブロックの先頭バイトを表示
    if score > 0.5 {
        let bytes = pack_bits_msb(&bits, bit_off);
        let mut di = ByteDeinterleaver::new();
        let mut out = Vec::new();
        for &b in &bytes[feed_off..] {
            let o = di.push(b);
            out.push(o);
        }
        let out = &out[latency..];
        eprint!("先頭ブロックの先頭バイト: ");
        let mut i = phase;
        let mut shown = 0;
        while i < out.len() && shown < 16 {
            eprint!("{:02x} ", out[i]);
            i += TSP;
            shown += 1;
        }
        eprintln!("(0x47=通常同期, 0xb8=反転同期)");
    }
}
