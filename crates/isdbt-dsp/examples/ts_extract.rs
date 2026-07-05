//! ⑥ 実電波 → MPEG-TS ファイル出力（バイトデインタ向き確定 → RS復号 → 逆拡散）。
//!
//! Viterbi → バイト化 → Forneyバイトデインタ → **RS(204,188)有効符号語率で整列・向きを確定** →
//! RS復号で188パケット化 → エネルギー逆拡散（リセット位相はPID集中度で総当たり）→ .ts。
//!
//! ```bash
//! cargo run -p isdbt-dsp --example ts_extract -- cap.iq 1015873 out.ts [nsym]
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
    best_sync_phase, pack_bits_msb, ByteDeinterleaver, EnergyPrbs, BI_I, BI_M, SYNC, TSP,
};
use isdbt_dsp::viterbi::{depuncture, Viterbi, PUNCTURE_2_3};
use num_complex::Complex32;
use std::{env, fs};

fn deinterleave(bytes: &[u8], commutator: usize, rev: bool, latency: usize) -> Vec<u8> {
    let mut di = ByteDeinterleaver::with_rev(rev);
    let mut out = Vec::with_capacity(bytes.len());
    for (j, &b) in bytes[commutator.min(bytes.len())..].iter().enumerate() {
        let o = di.push(b);
        if j >= latency {
            out.push(o);
        }
    }
    out
}

fn main() {
    let mut a = env::args().skip(1);
    let path = a
        .next()
        .expect("usage: ts_extract <cap.iq> <fs> <out.ts> [nsym]");
    let _fs: f64 = a.next().expect("fs").parse().unwrap();
    let outpath = a.next().expect("out.ts");
    let nsym: usize = a.next().map(|s| s.parse().unwrap()).unwrap_or(8000);

    let raw = fs::read(&path).expect("IQ");
    let mut s = u8_iq_to_complex(&raw);
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
    eprintln!(
        "Viterbi情報ビット {} (≈{}バイト)",
        bits.len(),
        bits.len() / 8
    );

    // 整列・向き確定：RS有効符号語率を最大化する (bit_off, commutator, rev, block_phase)
    let latency = BI_M * BI_I * (BI_I - 1);
    let checker = rs::Checker::new();
    let mut best = (0.0f32, 0usize, 0usize, false, 0usize);
    for &rev in &[false, true] {
        for bit_off in 0..8 {
            let bytes = pack_bits_msb(&bits, bit_off);
            if bytes.len() < latency + 60 * TSP {
                continue;
            }
            for commutator in 0..BI_I {
                let stream = deinterleave(&bytes, commutator, rev, latency);
                let (phase, sscore) = best_sync_phase(&stream);
                if sscore < 0.9 {
                    continue;
                }
                // RS復号成功率（先頭30ブロック）。正しい整列なら残留誤りをRSが拾える。
                let mut ok = 0usize;
                let mut tot = 0usize;
                let mut i = phase;
                while i + TSP <= stream.len() && tot < 30 {
                    let blk = &stream[i..i + TSP];
                    if checker.is_codeword(blk) || rs::decode(blk).is_some() {
                        ok += 1;
                    }
                    tot += 1;
                    i += TSP;
                }
                let frac = if tot > 0 { ok as f32 / tot as f32 } else { 0.0 };
                if frac > best.0 {
                    best = (frac, bit_off, commutator, rev, phase);
                }
            }
        }
    }
    let (rsfrac, bit_off, commutator, rev, phase) = best;
    eprintln!(
        "整列確定: bit_off={bit_off} commutator={commutator} rev={rev} block_phase={phase} → RS有効符号語率={:.1}%",
        rsfrac * 100.0
    );

    // 確定整列で全ブロック → RS復号 → 188パケット化
    let bytes = pack_bits_msb(&bits, bit_off);
    let stream = deinterleave(&bytes, commutator, rev, latency);
    let mut packets: Vec<Vec<u8>> = Vec::new();
    let mut rs_fixed = 0usize;
    let mut rs_fail = 0usize;
    let mut i = phase;
    while i + TSP <= stream.len() {
        let blk = &stream[i..i + TSP];
        let dec = rs::decode(blk);
        let payload = match &dec {
            Some(c) => {
                if c != blk {
                    rs_fixed += 1;
                }
                c[..188].to_vec()
            }
            None => {
                rs_fail += 1;
                blk[..188].to_vec()
            }
        };
        packets.push(payload); // [sync + 187 payload]（まだスクランブル）
        i += TSP;
    }
    eprintln!(
        "ブロック {} / RS訂正 {} / RS訂正不能 {}",
        packets.len(),
        rs_fixed,
        rs_fail
    );

    // エネルギー逆拡散：リセット位相・空クロックをPID集中度で総当たり
    let pid = |p: &[u8]| ((p[1] as u16 & 0x1f) << 8) | p[2] as u16;
    let concentration = |pkts: &[Vec<u8>]| -> f32 {
        let mut cnt = std::collections::HashMap::<u16, usize>::new();
        for p in pkts.iter().take(1500) {
            *cnt.entry(pid(p)).or_insert(0) += 1;
        }
        let n: usize = cnt.values().sum();
        let mut v: Vec<usize> = cnt.values().copied().collect();
        v.sort_unstable_by(|a, b| b.cmp(a));
        let top: usize = v.iter().take(4).sum();
        if n == 0 {
            0.0
        } else {
            top as f32 / n as f32
        }
    };
    let descramble = |init: u16, reset_off: usize, clock_sync: bool| -> Vec<Vec<u8>> {
        let mut prbs = EnergyPrbs::with_init(init);
        packets
            .iter()
            .enumerate()
            .map(|(idx, blk)| {
                if idx % 8 == reset_off {
                    prbs.reset_to(init);
                } else if clock_sync {
                    prbs.clock(8);
                }
                let mut p = Vec::with_capacity(188);
                p.push(SYNC);
                for &b in &blk[1..188] {
                    p.push(b ^ (prbs.clock(8) as u8));
                }
                p
            })
            .collect()
    };

    let mut bestd = (
        concentration(&packets),
        0xa9u16,
        0usize,
        false,
        packets.clone(),
    );
    for &init in &[0xa9u16, 0x4a80] {
        for reset_off in 0..8 {
            for &cs in &[false, true] {
                let d = descramble(init, reset_off, cs);
                let c = concentration(&d);
                if c > bestd.0 {
                    bestd = (c, init, reset_off, cs, d);
                }
            }
        }
    }
    let (conc, init, reset_off, cs, ts_pkts) = bestd;
    eprintln!(
        "逆拡散: init=0x{init:x} reset_off={reset_off} clock_sync={cs} PID集中度(上位4)={:.2}",
        conc
    );
    {
        let mut cnt = std::collections::HashMap::<u16, usize>::new();
        for p in ts_pkts.iter() {
            *cnt.entry(pid(p)).or_insert(0) += 1;
        }
        let mut v: Vec<(u16, usize)> = cnt.into_iter().collect();
        v.sort_unstable_by(|a, b| b.1.cmp(&a.1));
        eprint!("上位PID: ");
        for (p, n) in v.iter().take(8) {
            eprint!("0x{p:04x}({n}) ");
        }
        eprintln!();
    }

    let ts: Vec<u8> = ts_pkts.concat();
    fs::write(&outpath, &ts).expect("write ts");
    eprintln!("wrote {} ({} packets)", outpath, ts.len() / 188);
}
