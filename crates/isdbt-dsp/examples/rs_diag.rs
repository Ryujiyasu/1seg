//! RS/バイトデインタの切り分け診断。同期100%整列でシンドロームの様子を直接見る。
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

fn nzsynd(block: &[u8]) -> usize {
    rs::syndromes(block).iter().filter(|&&x| x != 0).count()
}

fn analyze(tag: &str, stream: &[u8]) {
    let (phase, score) = best_sync_phase(stream);
    if score < 0.5 {
        println!("{tag}: sync {:.1}% (整列なし)", score * 100.0);
        return;
    }
    let mut hist = [0usize; 17];
    let mut dec_ok = 0usize;
    let mut n = 0usize;
    let mut i = phase;
    while i + TSP <= stream.len() && n < 200 {
        let blk = &stream[i..i + TSP];
        hist[nzsynd(blk)] += 1;
        if rs::decode(blk).is_some() {
            dec_ok += 1;
        }
        n += 1;
        i += TSP;
    }
    println!(
        "{tag}: sync {:.1}% phase={phase} / {n}ブロック中 RS復号成功={dec_ok} / 非0シンドローム数ヒスト[0..8]={:?}",
        score * 100.0,
        &hist[0..9]
    );
}

fn main() {
    let mut a = env::args().skip(1);
    let path = a.next().unwrap();
    let _fs: f64 = a.next().unwrap().parse().unwrap();
    let nsym: usize = a.next().map(|s| s.parse().unwrap()).unwrap_or(4000);

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

    let latency = BI_M * BI_I * (BI_I - 1);
    let bytes = pack_bits_msb(&bits, 0);
    eprintln!("bytes={}", bytes.len());

    // RS規約の総当たり検証：生 phase=55 の先頭ブロックで、fcr×向き×ビット反転を試す
    let (rp, _) = best_sync_phase(&bytes);
    println!(
        "=== RS規約スキャン（生 phase={rp} の先頭ブロック, 非0シンドローム数, 小さいほど良）==="
    );
    for blkidx in 0..3 {
        let base = rp + blkidx * TSP;
        if base + TSP > bytes.len() {
            break;
        }
        let normal: Vec<u8> = bytes[base..base + TSP].to_vec();
        let mut rev = normal.clone();
        rev.reverse();
        let inv: Vec<u8> = normal.iter().map(|&b| !b).collect();
        for fcr in [0usize, 1] {
            println!(
                "  blk{blkidx} fcr={fcr}: normal={} reversed={} inverted={}",
                rs::nonzero_syndromes_fcr(&normal, fcr),
                rs::nonzero_syndromes_fcr(&rev, fcr),
                rs::nonzero_syndromes_fcr(&inv, fcr),
            );
        }
    }

    let _ = analyze; // 旧診断は未使用

    // 同時総当たり：デインタ(向き×コミュテータ) × RS規約(fcr×ブロック向き)
    // 各構成で先頭30ブロックの「非0シンドローム数の中央値」を測り、最小を報告。
    println!("=== 同時総当たり（非0シンドローム中央値, 0に近いほど正解）===");
    let mut best = (99i32, String::new());
    for rev in [false, true] {
        for c in 0..BI_I {
            let mut di = ByteDeinterleaver::with_rev(rev);
            let stream: Vec<u8> = bytes[c..]
                .iter()
                .enumerate()
                .filter_map(|(j, &b)| {
                    let o = di.push(b);
                    (j >= latency).then_some(o)
                })
                .collect();
            let (phase, score) = best_sync_phase(&stream);
            if score < 0.9 {
                continue;
            }
            for fcr in [0usize, 1] {
                for revblk in [false, true] {
                    let mut nzs: Vec<usize> = Vec::new();
                    let mut i = phase;
                    while i + TSP <= stream.len() && nzs.len() < 30 {
                        let mut blk = stream[i..i + TSP].to_vec();
                        if revblk {
                            blk.reverse();
                        }
                        nzs.push(rs::nonzero_syndromes_fcr(&blk, fcr));
                        i += TSP;
                    }
                    nzs.sort_unstable();
                    let med = nzs.get(nzs.len() / 2).copied().unwrap_or(16) as i32;
                    if med < best.0 {
                        best = (
                            med,
                            format!("rev={} c={c} fcr={fcr} revblk={revblk}", rev as u8),
                        );
                    }
                }
            }
        }
    }
    println!("最小中央値 = {} @ {}", best.0, best.1);
    println!(
        "{}",
        if best.0 <= 8 {
            "→ 訂正可能圏内の構成が見つかった"
        } else {
            "→ どの構成もRS圏外（デインタ/規約以外の要因＝ビット段の系統誤りを疑う）"
        }
    );
}
