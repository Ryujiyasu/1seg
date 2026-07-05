//! ① RF入力：rtl_sdr が吐く生IQ を複素サンプル列へ変換する。
//!
//! `rtl_sdr out.bin` の出力は **インターリーブされた 8bit 符号なし**
//! （I0,Q0,I1,Q1,…）で、中心は 127.5。これを -1.0..=1.0 付近の
//! `Complex32` に正規化する。

use num_complex::Complex32;

/// rtl_sdr の u8 IQ（I0,Q0,I1,Q1,…）を `Complex32` 列へ。
///
/// 端数（奇数バイト）は捨てる。
pub fn u8_iq_to_complex(bytes: &[u8]) -> Vec<Complex32> {
    const BIAS: f32 = 127.5;
    const SCALE: f32 = 1.0 / 127.5;
    bytes
        .chunks_exact(2)
        .map(|c| Complex32::new((c[0] as f32 - BIAS) * SCALE, (c[1] as f32 - BIAS) * SCALE))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn center_maps_to_zero_and_endpoints() {
        // 128 ≈ 中心 → ほぼ 0
        let s = u8_iq_to_complex(&[128, 128]);
        assert!(s[0].norm() < 0.01);
        // 255/0 → ほぼ +1/-1
        let s = u8_iq_to_complex(&[255, 0]);
        assert!((s[0].re - 1.0).abs() < 0.01);
        assert!((s[0].im + 1.0).abs() < 0.01);
    }

    #[test]
    fn odd_trailing_byte_is_dropped() {
        let s = u8_iq_to_complex(&[128, 128, 200]);
        assert_eq!(s.len(), 1);
    }
}
