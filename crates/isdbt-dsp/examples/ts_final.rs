//! ⑥ 完成へ：byte-deint → エネルギー逆拡散 → RS復号 → .ts。
//! 逆拡散モデル(周期・リセット位相・sync空クロック有無/前後・init)を総当たりし、
//! **RS復号成功**を指標に確定。最良構成では成功パターンと成功PIDをダンプして観察する。

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

/// 204ブロック列を逆拡散。sync(位置0)素通し、残り203をXOR。
/// `sync_clock`: 0=syncで空クロックしない, 1=前に8, 2=後に8。`period`ごとにPRBSリセット。
fn descramble(
    blocks: &[&[u8]],
    period: usize,
    reset_off: usize,
    init: u16,
    sync_clock: u8,
) -> Vec<Vec<u8>> {
    let mut prbs = EnergyPrbs::with_init(init);
    let mut out = Vec::with_capacity(blocks.len());
    for (idx, blk) in blocks.iter().enumerate() {
        if idx % period == reset_off % period {
            prbs.reset_to(init);
        }
        if sync_clock == 1 {
            prbs.clock(8);
        }
        let mut o = Vec::with_capacity(TSP);
        o.push(blk[0]);
        for &b in &blk[1..TSP] {
            o.push(b ^ (prbs.clock(8) as u8));
        }
        if sync_clock == 2 {
            prbs.clock(8);
        }
        out.push(o);
    }
    out
}

fn pid(p: &[u8]) -> u16 {
    ((p[1] as u16 & 0x1f) << 8) | p[2] as u16
}

fn main() {
    let mut a = env::args().skip(1);
    let path = a.next().unwrap();
    let _fs: f64 = a.next().unwrap().parse().unwrap();
    let outpath = a.next().unwrap();
    let nsym: usize = a.next().map(|s| s.parse().unwrap()).unwrap_or(8000);

    let rawf = fs::read(&path).unwrap();
    let mut s = u8_iq_to_complex(&rawf);
    let mean: Complex32 = s.iter().sum::<Complex32>() / s.len() as f32;
    for v in s.iter_mut() {
        *v -= mean;
    }
    let off = (s.len() / 10).min(50_000);
    let seg_sig = &s[off..];
    let est = estimate_sync(seg_sig, FFT_LEN).unwrap();
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
                coded.push(de[1]);
                coded.push(de[0]);
            }
        }
    }
    let restored = depuncture(&coded, &PUNCTURE_2_3);
    let take = restored.len() & !1;
    let bits = Viterbi::new().decode(&restored[..take]);
    let bytes = pack_bits_msb(&bits, 0);
    let latency = BI_M * BI_I * (BI_I - 1);

    // 全ブロック（byte-deint, commutator c）を作るヘルパ
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

    // 総当たり：commutator × period × reset_off × sync_clock × init、RS復号成功率
    let mut best = (0.0f32, 0usize, 8usize, 0usize, 0u8, 0xa9u16, 0usize);
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
        for period in [8usize, 12, 16, 20, 24, 32, 40, 48, 64, 96, 1_000_000] {
            let ncheck = 200.min(blocks.len());
            for reset_off in 0..period.min(24) {
                for sync_clock in [0u8, 1, 2] {
                    for init in [0xa9u16, 0x4a80] {
                        let ds = descramble(&blocks, period, reset_off, init, sync_clock);
                        let ok = ds
                            .iter()
                            .take(ncheck)
                            .filter(|b| rs::decode(b).is_some())
                            .count();
                        let f = ok as f32 / ncheck as f32;
                        if f > best.0 {
                            best = (f, c, period, reset_off, sync_clock, init, phase);
                        }
                    }
                }
            }
        }
    }
    let (frac, c, period, reset_off, sync_clock, init, phase) = best;
    eprintln!(
        "最良: commutator={c} period={period} reset_off={reset_off} sync_clock={sync_clock} init=0x{init:x} phase={phase} → RS成功率={:.0}%",
        frac * 100.0
    );

    // 最良構成で成功パターンとPIDをダンプ
    let stream = make_blocks(c);
    let blocks: Vec<&[u8]> = (0..)
        .map(|i| phase + i * TSP)
        .take_while(|&i| i + TSP <= stream.len())
        .map(|i| &stream[i..i + TSP])
        .collect();
    let ds = descramble(&blocks, period, reset_off, init, sync_clock);
    let pat: String = ds
        .iter()
        .take(48)
        .map(|b| if rs::decode(b).is_some() { '#' } else { '.' })
        .collect();
    eprintln!("RS成功パターン(先頭48): {pat}");
    eprint!("成功ブロックのPID: ");
    for b in ds.iter().take(200) {
        if let Some(cw) = rs::decode(b) {
            eprint!("0x{:04x} ", pid(&cw));
        }
    }
    eprintln!();

    if frac < 0.5 {
        eprintln!("（まだ低い。パターンから周期/ドリフトを読む）");
        return;
    }

    // .ts 書き出し：RS復号成功パケット(188)のみ。失敗は捨てる（映像を汚さない）。
    let mut ts: Vec<u8> = Vec::new();
    let mut nok = 0usize;
    for b in &ds {
        if let Some(cw) = rs::decode(b) {
            ts.extend_from_slice(&cw[..188]);
            nok += 1;
        }
    }
    fs::write(&outpath, &ts).unwrap();
    eprintln!(
        "wrote {outpath}: {nok}/{} パケット復号成功 ({:.1}%)",
        ds.len(),
        100.0 * nok as f32 / ds.len() as f32
    );
}
