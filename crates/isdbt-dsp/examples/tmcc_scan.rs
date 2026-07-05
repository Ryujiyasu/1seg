//! TMCCフレーム同期の「位相スキャン」をCSVダンプする（ブログ図用）。
//! 204通りのフレーム位相それぞれで、同期語(16bit)の一致率を測る。正しい位相で1つだけ立つ。
//!
//! ```bash
//! cargo run -p isdbt-dsp --example tmcc_scan -- cap.iq 1015873 out.csv [nsym]
//! ```

use isdbt_dsp::demod::OfdmDemod;
use isdbt_dsp::equalize::{extract_segment, SEGMENT_BIN_OFFSET};
use isdbt_dsp::iq::u8_iq_to_complex;
use isdbt_dsp::params::FFT_LEN;
use isdbt_dsp::sync::estimate_sync;
use isdbt_dsp::tmcc::{dbpsk_bits, SYMBOLS_PER_FRAME, SYNC_EVEN, SYNC_ODD, SYNC_SIZE};
use num_complex::Complex32;
use std::{env, fs};

fn main() {
    let mut a = env::args().skip(1);
    let path = a
        .next()
        .expect("usage: tmcc_scan <cap.iq> <fs> <out.csv> [nsym]");
    let _fs: f64 = a.next().expect("fs").parse().unwrap();
    let out = a.next().expect("out.csv");
    let nsym: usize = a.next().map(|s| s.parse().unwrap()).unwrap_or(2400);

    let bytes = fs::read(&path).expect("IQ");
    let mut s = u8_iq_to_complex(&bytes);
    let mean: Complex32 = s.iter().sum::<Complex32>() / s.len() as f32;
    for v in s.iter_mut() {
        *v -= mean;
    }
    let off = (s.len() / 10).min(50_000);
    let seg_sig = &s[off..];

    let est = estimate_sync(seg_sig, FFT_LEN).expect("sync");
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
    let bits = dbpsk_bits(&segs);

    let mut csv = String::from("phase,match\n");
    for phase in 0..SYMBOLS_PER_FRAME {
        let mut matched = 0usize;
        let mut total = 0usize;
        let mut f = 0usize;
        loop {
            let start = phase + f * SYMBOLS_PER_FRAME;
            if start + 1 + SYNC_SIZE > bits.len() {
                break;
            }
            let w = &bits[start + 1..start + 1 + SYNC_SIZE];
            let me = w.iter().zip(&SYNC_EVEN).filter(|(a, b)| a == b).count();
            let mo = w.iter().zip(&SYNC_ODD).filter(|(a, b)| a == b).count();
            matched += me.max(mo);
            total += SYNC_SIZE;
            f += 1;
        }
        let m = matched as f32 / total.max(1) as f32;
        csv.push_str(&format!("{phase},{m:.4}\n"));
    }
    fs::write(&out, csv).expect("write");
    eprintln!("wrote {out} (204 phases)");
}
