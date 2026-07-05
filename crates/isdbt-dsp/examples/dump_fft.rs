//! 実IQをロック→復調し、副搬送波スペクトル行列をバイナリ出力する（可視化用）。
//!
//! ```bash
//! cargo run -p isdbt-dsp --example dump_fft -- cap.iq 1015873 /path/out
//! # → out.f32 (nsym×N×2 の f32 LE) と out.meta を書く
//! ```

use isdbt_dsp::demod::OfdmDemod;
use isdbt_dsp::iq::u8_iq_to_complex;
use isdbt_dsp::params::FFT_LEN;
use isdbt_dsp::sync::estimate_sync;
use num_complex::Complex32;
use std::io::Write;
use std::{env, fs};

fn main() {
    let mut a = env::args().skip(1);
    let path = a.next().expect("usage: dump_fft <cap.iq> <fs_hz> <out_prefix>");
    let fs: f64 = a.next().expect("fs_hz").parse().expect("fs数値");
    let out = a.next().expect("out_prefix");
    let nsym: usize = a.next().map(|s| s.parse().unwrap()).unwrap_or(256);

    let bytes = fs::read(&path).expect("IQ読み込み");
    let mut s = u8_iq_to_complex(&bytes);
    let mean: Complex32 = s.iter().sum::<Complex32>() / s.len() as f32;
    for v in s.iter_mut() {
        *v -= mean;
    }

    // 解析窓（先頭は過渡やAGC変動があるので少し進めた所から）
    let off = (s.len() / 10).min(50_000);
    let seg = &s[off..];
    let est = estimate_sync(seg, FFT_LEN).expect("同期できない");
    eprintln!(
        "sync: start={} gi={:?} metric={:.3} cfo={:.4}sc ({:.1}Hz)",
        est.symbol_start,
        est.guard,
        est.metric,
        est.cfo_subcarriers,
        est.cfo_subcarriers as f64 * fs / FFT_LEN as f64
    );

    let demod = OfdmDemod::new(FFT_LEN);
    let specs = demod.demod_stream(seg, est.symbol_start, est.guard, est.cfo_subcarriers, nsym);
    let got = specs.len();
    eprintln!("復調シンボル数: {got} (N={FFT_LEN})");

    // バイナリ出力（sym×N×2 f32 LE）
    let mut buf = Vec::with_capacity(got * FFT_LEN * 2 * 4);
    for sp in &specs {
        for c in sp {
            buf.extend_from_slice(&c.re.to_le_bytes());
            buf.extend_from_slice(&c.im.to_le_bytes());
        }
    }
    fs::write(format!("{out}.f32"), &buf).expect("f32書き込み");

    let meta = format!(
        "nsym={got}\nn={FFT_LEN}\nfs={fs}\ncfo_sc={}\ngi={:?}\nsymbol_start={}\n",
        est.cfo_subcarriers, est.guard, est.symbol_start
    );
    let mut f = fs::File::create(format!("{out}.meta")).unwrap();
    f.write_all(meta.as_bytes()).unwrap();
    eprintln!("wrote {out}.f32 / {out}.meta");
}
