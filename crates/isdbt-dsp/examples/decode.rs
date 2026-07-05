//! ISDB-T 1セグ **エンドツーエンド復調器**：IQ → MPEG-TS（単一パス, 確定パラメータ）。
//!
//! これがプロジェクトの本線。診断用の probe 群（ts_probe/ts_final/rs_diag…）で突き止めた
//! 確定パラメータを固定し、キャプチャ依存の整列だけを最小探索する。
//!
//! 段：② OFDM同期 → ③ SP等化 → ④ 周波数/時間/ビットデインタ＋QPSKデマップ →
//!     ⑤ Viterbi(2/3)＋RS(204,188) → ⑥ エネルギー逆拡散 → TS。
//!
//! **確定パラメータ**（[[この capture 系列]]で不変）：
//! - FEC符号ビット順 order=1（キャリアあたり [msb,lsb]）, パンクチャ位相 pphase=0
//! - byte-deint 標準向き, RXチェーン順は byte-deint→**逆拡散→RS**（ISDB-TはRS後にスクランブル）
//! - エネルギー逆拡散 PRBS init=0xa9, sync位置は203バイトXORの**後**に8空クロック
//! - **リセット周期 = 64 RSブロック = 1 OFDMフレーム**（204sym×384carrier×2bit×2/3÷8÷204=64）
//!
//! ```bash
//! cargo run --release -p isdbt-dsp --example decode -- cap.iq 1015873 out.ts [nsym]
//! ffmpeg -ss 5 -i out.ts -frames:v 1 frame.png
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
use isdbt_dsp::rs;
use isdbt_dsp::sync::estimate_sync;
use isdbt_dsp::ts::{
    best_sync_phase, pack_bits_msb, ByteDeinterleaver, EnergyPrbs, BI_I, BI_M, TSP,
};
use isdbt_dsp::viterbi::{depuncture, Viterbi, PUNCTURE_2_3};
use num_complex::Complex32;
use std::{env, fs};

/// エネルギー拡散のリセット周期＝1 OFDMフレームぶんのRSブロック数。
const RESET_PERIOD: usize = 64;
const PRBS_INIT: u16 = 0xa9;

/// 204ブロック列を逆拡散（sync素通し・残り203をXOR・203XORの後にsync分8空クロック・
/// `RESET_PERIOD` ごとに PRBS を init へ）。`reset_off` はフレーム先頭ブロックの位相。
fn descramble(blocks: &[&[u8]], reset_off: usize) -> Vec<Vec<u8>> {
    let mut prbs = EnergyPrbs::with_init(PRBS_INIT);
    let mut out = Vec::with_capacity(blocks.len());
    for (idx, blk) in blocks.iter().enumerate() {
        if idx % RESET_PERIOD == reset_off % RESET_PERIOD {
            prbs.reset_to(PRBS_INIT);
        }
        let mut o = Vec::with_capacity(TSP);
        o.push(blk[0]);
        for &b in &blk[1..TSP] {
            o.push(b ^ (prbs.clock(8) as u8));
        }
        prbs.clock(8); // sync位置ぶん
        out.push(o);
    }
    out
}

fn main() {
    let mut a = env::args().skip(1);
    let path = a
        .next()
        .expect("usage: decode <cap.iq> <fs_hz> <out.ts> [nsym]");
    let _fs: f64 = a.next().expect("fs").parse().expect("fs");
    let outpath = a.next().expect("out.ts");
    let nsym: usize = a.next().map(|s| s.parse().unwrap()).unwrap_or(11000);

    // ① IQ → DC除去
    let rawf = fs::read(&path).expect("IQ読み込み");
    let mut s = u8_iq_to_complex(&rawf);
    let mean: Complex32 = s.iter().sum::<Complex32>() / s.len() as f32;
    for v in s.iter_mut() {
        *v -= mean;
    }
    let off = (s.len() / 10).min(50_000);
    let seg_sig = &s[off..];

    // ② 同期 → 復調
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
    let (phase0, _) = detect_symbol_phase(&extract_segment(&specs[0], SEGMENT_BIN_OFFSET), &pilots);

    // ③④ 等化 → データ抽出 → 周波数/時間デインタ → QPSKソフトデマップ → ビットデインタ
    //   → 確定順(order=1)で符号ソフト列
    let mut tdi = TimeDeinterleaver::new(4);
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

    // ⑤ depuncture(2/3, pphase=0) → Viterbi → 情報ビット → バイト化
    let restored = depuncture(&coded, &PUNCTURE_2_3);
    let take = restored.len() & !1;
    let bits = Viterbi::new().decode(&restored[..take]);
    let bytes = pack_bits_msb(&bits, 0);
    let latency = BI_M * BI_I * (BI_I - 1);

    let make_blocks = |c: usize| -> Vec<u8> {
        let mut di = ByteDeinterleaver::new();
        bytes[c..]
            .iter()
            .enumerate()
            .filter_map(|(j, &b)| {
                let o = di.push(b);
                (j >= latency).then_some(o)
            })
            .collect()
    };

    // 整列（キャプチャ依存）：commutator × フレーム位相 reset_off を RS成功で最小探索
    let mut best = (0.0f32, 0usize, 0usize, 0usize); // frac, commutator, reset_off, phase
    for c in 0..BI_I {
        let stream = make_blocks(c);
        let (phase, sc) = best_sync_phase(&stream);
        if sc < 0.9 {
            continue;
        }
        let blocks: Vec<&[u8]> = (0..)
            .map(|i| phase + i * TSP)
            .take_while(|&i| i + TSP <= stream.len())
            .map(|i| &stream[i..i + TSP])
            .collect();
        let ncheck = 128.min(blocks.len());
        for reset_off in 0..RESET_PERIOD {
            let ds = descramble(&blocks, reset_off);
            let ok = ds
                .iter()
                .take(ncheck)
                .filter(|b| rs::decode(b).is_some())
                .count();
            let f = ok as f32 / ncheck as f32;
            if f > best.0 {
                best = (f, c, reset_off, phase);
            }
        }
    }
    let (frac, commutator, reset_off, phase) = best;
    eprintln!(
        "② start={} / 復調{}シンボル / 整列: commutator={commutator} reset_off={reset_off} block_phase={phase} → RS成功率≈{:.1}%",
        est.symbol_start,
        specs.len(),
        frac * 100.0
    );
    if frac < 0.5 {
        eprintln!("✗ 復号に失敗（信号品質 or 整列）。");
        return;
    }

    // ⑥ 逆拡散 → RS復号 → 成功パケット(188)だけ書き出し
    let stream = make_blocks(commutator);
    let blocks: Vec<&[u8]> = (0..)
        .map(|i| phase + i * TSP)
        .take_while(|&i| i + TSP <= stream.len())
        .map(|i| &stream[i..i + TSP])
        .collect();
    let ds = descramble(&blocks, reset_off);
    let mut ts: Vec<u8> = Vec::new();
    let mut nok = 0usize;
    for b in &ds {
        if let Some(cw) = rs::decode(b) {
            ts.extend_from_slice(&cw[..188]);
            nok += 1;
        }
    }
    fs::write(&outpath, &ts).expect("TS書き込み");
    println!(
        "✅ {outpath} : {nok}/{} パケット復号 ({:.1}%) → {} バイトのMPEG-TS",
        ds.len(),
        100.0 * nok as f32 / ds.len().max(1) as f32,
        ts.len()
    );
    println!("   確認: ffprobe {outpath} / ffmpeg -ss 5 -i {outpath} -frames:v 1 frame.png");
}
