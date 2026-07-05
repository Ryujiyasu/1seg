//! ISDB-T 1セグ **ストリーミング復調器 CLI**（連続ライブ対応）。
//! IQ(stdin/file) → MPEG-TS(stdout/file) を、届いた端から復号して吐き続ける薄いラッパー。
//! 中核は [`isdbt_dsp::StreamingDecoder`]。
//!
//! ライブ再生（表示のある端末で）：
//! ```bash
//! rtl_sdr -f 497142857 -s 1015873 -g 30 - | \
//!   cargo run --release -q -p isdbt-dsp --example stream_decode -- - - | ffplay -
//! ```

use isdbt_dsp::StreamingDecoder;
use std::io::{Read, Write};
use std::{env, fs};

fn main() {
    let mut a = env::args().skip(1);
    let inpath = a.next().expect("usage: stream_decode <in|-> <out|->");
    let outpath = a.next().expect("out");

    let mut reader: Box<dyn Read> = if inpath == "-" {
        Box::new(std::io::stdin().lock())
    } else {
        Box::new(fs::File::open(&inpath).expect("in"))
    };
    let mut out: Box<dyn Write> = if outpath == "-" {
        Box::new(std::io::stdout().lock())
    } else {
        Box::new(fs::File::create(&outpath).expect("out"))
    };

    let mut dec = StreamingDecoder::new();
    let mut raw = vec![0u8; 1 << 18];
    let mut announced = false;
    loop {
        let n = match reader.read(&mut raw) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let ts = dec.feed(&raw[..n]);
        if !ts.is_empty() {
            let _ = out.write_all(&ts);
            let _ = out.flush();
            if !announced {
                announced = true;
                eprintln!("ロック→ライブ復号開始");
            }
        }
    }
    let (ndec, nblk) = dec.stats();
    eprintln!(
        "終了: {ndec}/{nblk} ブロック復号 ({:.1}%)",
        100.0 * ndec as f32 / nblk.max(1) as f32
    );
}
