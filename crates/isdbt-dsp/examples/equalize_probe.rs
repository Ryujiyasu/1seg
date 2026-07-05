//! 実IQをロック→復調→③等化し、コンステレーションを書き出す診断ツール。
//!
//! やること：
//! 1. ②同期して各OFDMシンボルをFFT（fftshift済み1024本）
//! 2. 中央セグメント432本の **bin整列** と **symbol%4 位相** を、SPコヒーレンス
//!    最大化で経験的に確定（PRBSオフセット2592を含む仮説全体の検算にもなる）
//! 3. 確定した整列で各シンボルを等化し、データキャリアのコンステを蓄積・出力
//!
//! ```bash
//! cargo run -p isdbt-dsp --example equalize_probe -- cap.iq 1015873 /path/out [nsym]
//! # → out.const.f32 (点×2 f32 LE) と out.meta
//! ```

use isdbt_dsp::demod::OfdmDemod;
use isdbt_dsp::equalize::{
    detect_symbol_phase, equalize, estimate_channel, extract_segment, sp_coherence,
    SEGMENT_BIN_OFFSET,
};
use isdbt_dsp::iq::u8_iq_to_complex;
use isdbt_dsp::params::FFT_LEN;
use isdbt_dsp::pilots::SegmentPilots;
use isdbt_dsp::sync::estimate_sync;
use num_complex::Complex32;
use std::collections::HashSet;
use std::io::Write;
use std::{env, fs};

fn main() {
    let mut a = env::args().skip(1);
    let path = a
        .next()
        .expect("usage: equalize_probe <cap.iq> <fs_hz> <out_prefix> [nsym]");
    let fs: f64 = a.next().expect("fs_hz").parse().expect("fs数値");
    let out = a.next().expect("out_prefix");
    let nsym: usize = a.next().map(|s| s.parse().unwrap()).unwrap_or(512);

    // ① IQ読み込み＋DC除去
    let bytes = fs::read(&path).expect("IQ読み込み");
    let mut s = u8_iq_to_complex(&bytes);
    let mean: Complex32 = s.iter().sum::<Complex32>() / s.len() as f32;
    for v in s.iter_mut() {
        *v -= mean;
    }
    let off = (s.len() / 10).min(50_000);
    let seg_sig = &s[off..];

    // ② 同期
    let est = estimate_sync(seg_sig, FFT_LEN).expect("同期できない");
    eprintln!(
        "② sync: start={} gi={:?} metric={:.3} cfo={:.4}sc ({:.1}Hz)",
        est.symbol_start,
        est.guard,
        est.metric,
        est.cfo_subcarriers,
        est.cfo_subcarriers as f64 * fs / FFT_LEN as f64
    );

    // 復調（fftshift済み 1024×nsym）
    let demod = OfdmDemod::new(FFT_LEN);
    let specs = demod.demod_stream(
        seg_sig,
        est.symbol_start,
        est.guard,
        est.cfo_subcarriers,
        nsym,
    );
    eprintln!("復調シンボル数: {}", specs.len());
    assert!(specs.len() >= 8, "シンボルが足りない");

    let pilots = SegmentPilots::center_1seg();

    // ③-a bin整列の経験的確定：bin_offset を 296±8 で振り、最初の8シンボルの
    //     「位相最良コヒーレンス」総和が最大の bin_offset を選ぶ。
    let probe_syms = 8.min(specs.len());
    let center = SEGMENT_BIN_OFFSET as isize;
    let mut best_off = SEGMENT_BIN_OFFSET;
    let mut best_score = -1.0f32;
    let mut scan: Vec<(usize, f32)> = Vec::new();
    eprintln!("\n③ bin整列スキャン（bin_offset → Σ位相最良コヒーレンス, {probe_syms}シンボル）:");
    for d in -8isize..=8 {
        let bo = (center + d) as usize;
        let mut score = 0.0f32;
        for sp in specs.iter().take(probe_syms) {
            let seg = extract_segment(sp, bo);
            let (_p, coh) = detect_symbol_phase(&seg, &pilots);
            score += coh;
        }
        let avg = score / probe_syms as f32;
        scan.push((bo, avg));
        if d.abs() <= 2 || score > best_score {
            eprintln!("   bin_offset={bo:3} (Δ{d:+}) : {avg:.3}");
        }
        if score > best_score {
            best_score = score;
            best_off = bo;
        }
    }
    // スキャン結果をCSVで保存（可視化用）
    let scan_csv: String = scan
        .iter()
        .map(|(bo, a)| format!("{bo},{a:.4}\n"))
        .collect();
    fs::write(format!("{out}.scan.csv"), scan_csv).expect("scan書き込み");
    eprintln!(
        "→ 採用 bin_offset={best_off} (平均コヒーレンス {:.3})",
        best_score / probe_syms as f32
    );

    // ③-b symbol%4 位相：各シンボルを独立検出し、+1ずつ進む整合性を確認
    let phases: Vec<(usize, f32)> = specs
        .iter()
        .take(probe_syms)
        .map(|sp| detect_symbol_phase(&extract_segment(sp, best_off), &pilots))
        .collect();
    eprintln!("\n位相系列（symbol%4, coherence）先頭{probe_syms}:");
    for (i, (p, c)) in phases.iter().enumerate() {
        eprintln!("   sym{i}: %4={p} coh={c:.3}");
    }
    let phase0 = phases[0].0;
    let consistent = phases
        .iter()
        .enumerate()
        .all(|(i, (p, _))| *p == (phase0 + i) % 4);
    eprintln!(
        "位相が +1/シンボルで整合: {}",
        if consistent {
            "はい ✓（同期＋整列が正しい強い証拠）"
        } else {
            "いいえ ✗"
        }
    );

    // ③-c 全シンボル等化、データキャリア（SP除外）のコンステを蓄積。
    //     合わせてシンボルごとのSPコヒーレンス（C/Nの代理指標）も記録し、
    //     高コヒーレンスのシンボルだけを別ファイルにも出す（C/N依存の確認用）。
    let mut points: Vec<Complex32> = Vec::new();
    let mut points_good: Vec<Complex32> = Vec::new();
    let mut coh_sum = 0.0f32;
    let coh_n = specs.len();
    const GOOD_COH: f32 = 0.90; // この値以上のシンボルを「良C/N」とみなす
    let mut good_syms = 0usize;
    for (i, sp) in specs.iter().enumerate() {
        let seg = extract_segment(sp, best_off);
        let sym_mod4 = (phase0 + i) % 4;
        let coh = sp_coherence(&seg, sym_mod4, &pilots);
        coh_sum += coh;
        let good = coh >= GOOD_COH;
        if good {
            good_syms += 1;
        }

        let h = estimate_channel(&seg, sym_mod4, &pilots);
        let xeq = equalize(&seg, &h);
        let sp_set: HashSet<usize> = pilots.sp_carriers(sym_mod4).collect();
        for (l, x) in xeq.iter().enumerate() {
            if !sp_set.contains(&l) {
                points.push(*x);
                if good {
                    points_good.push(*x);
                }
            }
        }
    }
    let mean_coh = coh_sum / coh_n as f32;
    eprintln!("\n平均SPコヒーレンス（全{coh_n}シンボル）: {mean_coh:.3}");
    eprintln!("良C/Nシンボル(coh≥{GOOD_COH}): {good_syms}/{coh_n}");
    eprintln!("コンステ点数（データキャリア, SP除外）: {}", points.len());

    // 等化後データのRMS（QPSK/QAMなら ~1 のはず）
    let rms = (points.iter().map(|c| c.norm_sqr()).sum::<f32>() / points.len() as f32).sqrt();
    eprintln!("等化後データ振幅RMS: {rms:.3}");

    // 出力（点×2 f32 LE）
    let mut buf = Vec::with_capacity(points.len() * 8);
    for c in &points {
        buf.extend_from_slice(&c.re.to_le_bytes());
        buf.extend_from_slice(&c.im.to_le_bytes());
    }
    fs::write(format!("{out}.const.f32"), &buf).expect("const書き込み");

    let mut bufg = Vec::with_capacity(points_good.len() * 8);
    for c in &points_good {
        bufg.extend_from_slice(&c.re.to_le_bytes());
        bufg.extend_from_slice(&c.im.to_le_bytes());
    }
    fs::write(format!("{out}.const_good.f32"), &bufg).expect("const_good書き込み");

    let meta = format!(
        "points={}\nbin_offset={best_off}\nphase0={phase0}\nmean_coherence={mean_coh:.4}\n\
         phase_consistent={consistent}\nnsym={}\ngi={:?}\ncfo_sc={}\nfs={fs}\nrms={rms:.4}\n",
        points.len(),
        specs.len(),
        est.guard,
        est.cfo_subcarriers,
    );
    let mut f = fs::File::create(format!("{out}.meta")).unwrap();
    f.write_all(meta.as_bytes()).unwrap();
    eprintln!("wrote {out}.const.f32 / {out}.meta");
}
