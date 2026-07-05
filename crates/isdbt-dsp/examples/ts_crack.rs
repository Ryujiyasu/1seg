//! ⑥段は正しいと判明したので、上流のビット規約（order/shift/pphase/xyswap/invert）を
//! 総当たりし、**RS有効ブロックが立つ**構成を探す。RSが最終判定器。

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
use isdbt_dsp::ts::{best_sync_phase, pack_bits_msb, ByteDeinterleaver, BI_I, BI_M, TSP};
use isdbt_dsp::viterbi::{depuncture, Viterbi, PUNCTURE_2_3};
use num_complex::Complex32;
use std::{env, fs};

fn main() {
    let mut a = env::args().skip(1);
    let path = a.next().unwrap();
    let _fs: f64 = a.next().unwrap().parse().unwrap();
    let nsym: usize = a.next().map(|s| s.parse().unwrap()).unwrap_or(3000);

    let raw = fs::read(&path).unwrap();
    let mut s = u8_iq_to_complex(&raw);
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

    // per-carrier soft [lsb, msb] を一度だけ用意
    let mut tdi = TimeDeinterleaver::new(4);
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
    eprintln!("carriers={}", carriers.len());

    let v = Viterbi::new();
    let checker = rs::Checker::new();
    let latency = BI_M * BI_I * (BI_I - 1);
    let max_steps = 60_000usize;

    let mut found = false;
    for order in 0..2 {
        // per-carrier soft を order で直列化
        let mut ser: Vec<f32> = Vec::with_capacity(carriers.len() * 2);
        for c in &carriers {
            if order == 0 {
                ser.push(c[0]);
                ser.push(c[1]);
            } else {
                ser.push(c[1]);
                ser.push(c[0]);
            }
        }
        for invert in 0..2 {
            let base: Vec<f32> = if invert == 1 {
                ser.iter().map(|&x| -x).collect()
            } else {
                ser.clone()
            };
            for shift in 0..2 {
                let sl = &base[shift..];
                for pphase in 0..4 {
                    if pphase >= sl.len() {
                        continue;
                    }
                    let punct = &sl[pphase..];
                    let mother = depuncture(punct, &PUNCTURE_2_3);
                    for xyswap in 0..2 {
                        let mut m = mother.clone();
                        if xyswap == 1 {
                            let n = m.len() & !1;
                            for i in (0..n).step_by(2) {
                                m.swap(i, i + 1);
                            }
                        }
                        let take = (max_steps * 2).min(m.len() & !1);
                        if take < 4000 {
                            continue;
                        }
                        let bits = v.decode(&m[..take]);
                        // bit_off × commutator を掃いて RS有効率を測る
                        for bit_off in 0..8 {
                            let bytes = pack_bits_msb(&bits, bit_off);
                            if bytes.len() < latency + 30 * TSP {
                                continue;
                            }
                            for c in 0..BI_I {
                                let mut di = ByteDeinterleaver::new();
                                let stream: Vec<u8> = bytes[c..]
                                    .iter()
                                    .enumerate()
                                    .filter_map(|(j, &b)| {
                                        let o = di.push(b);
                                        (j >= latency).then_some(o)
                                    })
                                    .collect();
                                let (phase, sc) = best_sync_phase(&stream);
                                if sc < 0.9 {
                                    continue;
                                }
                                let mut ok = 0;
                                let mut tot = 0;
                                let mut i = phase;
                                while i + TSP <= stream.len() && tot < 25 {
                                    let blk = &stream[i..i + TSP];
                                    if checker.is_codeword(blk) || rs::decode(blk).is_some() {
                                        ok += 1;
                                    }
                                    tot += 1;
                                    i += TSP;
                                }
                                if tot > 0 && ok * 3 >= tot {
                                    println!(
                                        "★ order={order} invert={invert} shift={shift} pphase={pphase} xyswap={xyswap} bit_off={bit_off} commutator={c} → RS有効 {ok}/{tot} (phase={phase})"
                                    );
                                    found = true;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    if !found {
        println!("✗ どのビット規約でもRSが立たなかった（さらに上流を疑う）");
    }
}
