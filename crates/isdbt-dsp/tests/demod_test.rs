//! OFDM復調の単体検証：周波数領域に立てた単一トーンが、
//! IFFT→CP付与→`demod_stream` を通って、正しいfftshift位置に戻るか。

use isdbt_dsp::demod::OfdmDemod;
use isdbt_dsp::params::GuardInterval;
use num_complex::Complex32;
use rustfft::FftPlanner;

#[test]
fn single_tone_round_trips_to_shifted_bin() {
    let n = 64usize;
    let gi = GuardInterval::G1_4;
    let l = gi.cp_len(n); // 16
    let k0 = 5usize; // 立てるキャリア

    // 周波数領域: bin k0 だけ 1.0
    let mut freq = vec![Complex32::new(0.0, 0.0); n];
    freq[k0] = Complex32::new(1.0, 0.0);

    // 時間領域へ（IFFT）
    let mut planner = FftPlanner::<f32>::new();
    let ifft = planner.plan_fft_inverse(n);
    let mut time = freq.clone();
    ifft.process(&mut time);

    // CP付与（末尾L個を先頭へ）して1シンボル分の信号に
    let mut sig: Vec<Complex32> = Vec::with_capacity(n + l);
    sig.extend_from_slice(&time[n - l..]);
    sig.extend_from_slice(&time);

    let demod = OfdmDemod::new(n);
    let specs = demod.demod_stream(&sig, 0, gi, 0.0, 1);
    assert_eq!(specs.len(), 1);
    let spec = &specs[0];

    // fftshift後の期待index
    let expected = (k0 + n / 2) % n;
    let argmax = (0..n)
        .max_by(|&a, &b| spec[a].norm().partial_cmp(&spec[b].norm()).unwrap())
        .unwrap();
    assert_eq!(argmax, expected, "トーンが想定bin({expected})に戻っていない");

    // そのbinにエネルギーが集中しているか（他binの合計より十分大きい）
    let peak = spec[expected].norm();
    let rest: f32 = (0..n).filter(|&k| k != expected).map(|k| spec[k].norm()).sum();
    assert!(peak > rest * 10.0, "ピークが立っていない peak={peak}, rest={rest}");
}
