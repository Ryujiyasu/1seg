//! ② OFDM同期の結合テスト。
//!
//! 合成OFDM信号（QPSK×1024キャリア＋CP）に、既知のCFOと前置きオフセットと
//! ノイズを加え、`estimate_symbol_sync` が
//!   - シンボル境界を正しく当てる
//!   - 小数CFOを復元する
//! ことを確認する。実IQが録れる前から、ここで土台を検証できる。

use isdbt_dsp::params::GuardInterval;
use isdbt_dsp::sync::{estimate_symbol_sync, estimate_sync};
use num_complex::Complex32;
use rustfft::FftPlanner;
use std::f32::consts::{FRAC_1_SQRT_2, PI};

/// 決定論的な簡易PRNG（xorshift64）。テストを再現可能にする。
struct Rng(u64);
impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    /// 上位ビットから 1bit（下位ビットは品質が低いため）。
    fn bit(&mut self) -> f32 {
        ((self.next_u64() >> 33) & 1) as f32
    }
    fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
    /// 標準正規（Box-Muller）。
    fn gaussian(&mut self) -> f32 {
        let u1 = self.unit().max(1e-7);
        let u2 = self.unit();
        (-2.0 * u1.ln()).sqrt() * (2.0 * PI * u2).cos()
    }
}

/// QPSK×Nキャリアの合成OFDM（CP付き）を `num_sym` シンボル分つくる。
fn gen_ofdm(n: usize, l: usize, num_sym: usize, rng: &mut Rng) -> Vec<Complex32> {
    let mut planner = FftPlanner::<f32>::new();
    let ifft = planner.plan_fft_inverse(n);
    let scale = 1.0 / (n as f32).sqrt(); // rustfftは未正規化
    let mut out = Vec::with_capacity(num_sym * (n + l));
    for _ in 0..num_sym {
        let mut freq: Vec<Complex32> = (0..n)
            .map(|_| Complex32::new(FRAC_1_SQRT_2 * (2.0 * rng.bit() - 1.0),
                                    FRAC_1_SQRT_2 * (2.0 * rng.bit() - 1.0)))
            .collect();
        ifft.process(&mut freq);
        for v in freq.iter_mut() {
            *v *= scale;
        }
        // CP付与：有効部の末尾L個を先頭へコピー
        out.extend_from_slice(&freq[n - l..]);
        out.extend_from_slice(&freq);
    }
    out
}

#[test]
fn sync_finds_boundary_and_cfo() {
    let n = 1024usize;
    let gi = GuardInterval::G1_8;
    let l = gi.cp_len(n); // 128
    let sym = n + l; // 1152

    let mut rng = Rng(0x1234_5678_9abc_def0);

    // 先頭にランダムな前置きを入れて、境界を 0 からずらす
    let pre = 500usize;
    let mut sig: Vec<Complex32> = (0..pre)
        .map(|_| Complex32::new(rng.gaussian() * 0.3, rng.gaussian() * 0.3))
        .collect();
    sig.extend(gen_ofdm(n, l, 8, &mut rng));

    // 既知のCFO（キャリア間隔単位）を印加： r[m] *= exp(j2π ε m / N)
    let eps = 0.17f32;
    for (m, v) in sig.iter_mut().enumerate() {
        *v *= Complex32::from_polar(1.0, 2.0 * PI * eps * (m as f32) / (n as f32));
    }

    // 軽いノイズ
    let noise = 0.05f32;
    for v in sig.iter_mut() {
        *v += Complex32::new(rng.gaussian() * noise, rng.gaussian() * noise);
    }

    let est = estimate_symbol_sync(&sig, n, gi).expect("十分なサンプル数");

    // 境界：(symbol_start - pre) が sym の倍数付近か
    let rel = ((est.symbol_start as i64 - pre as i64) % sym as i64 + sym as i64) % sym as i64;
    let dist = rel.min(sym as i64 - rel);
    assert!(
        dist <= 2,
        "symbol_start={} が境界からズレ過ぎ (dist={}, metric={})",
        est.symbol_start, dist, est.metric
    );

    // CFO：0.17 を当てる（van de Beek は ±0.5 まで一意）
    assert!(
        (est.cfo_subcarriers - eps).abs() < 0.03,
        "cfo_subcarriers={}（期待 {})", est.cfo_subcarriers, eps
    );

    // 相関ピークは十分高いはず
    assert!(est.metric > 0.7, "metric={} が低い", est.metric);
}

#[test]
fn estimate_sync_picks_correct_guard_by_periodicity() {
    // GI=1/8 で作った信号から、周期性ベースの検出が 1/8 を当てるか。
    // （単発メトリクス最大だと最短GI 1/32 に誤判定しがち）
    let n = 1024usize;
    let gi = GuardInterval::G1_8;
    let l = gi.cp_len(n);

    let mut rng = Rng(0xdead_beef_cafe_0001);
    let pre = 300usize;
    let mut sig: Vec<Complex32> = (0..pre)
        .map(|_| Complex32::new(rng.gaussian() * 0.3, rng.gaussian() * 0.3))
        .collect();
    sig.extend(gen_ofdm(n, l, 40, &mut rng)); // 折り畳みに足る symbol 数
    for v in sig.iter_mut() {
        *v += Complex32::new(rng.gaussian() * 0.05, rng.gaussian() * 0.05);
    }

    let est = estimate_sync(&sig, n).expect("十分なサンプル");
    assert_eq!(est.guard, GuardInterval::G1_8, "GIを周期性で当てられていない");
}
