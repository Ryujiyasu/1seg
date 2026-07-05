//! ISDB-T 1セグ（単独受信・Mode 3 相当）の物理層パラメータ。
//!
//! フルセグの 8192点FFT のうち中央1セグメントだけを 1/8 レートで切り出して
//! 単独復調する構成（gr-isdbt / K's Memo-Random の 1seg 実装に倣う）。
//! 一次資料は ARIB STD-B31。

/// 1セグ単独受信での有効シンボル長（FFTサイズ）。フルセグ Mode3 = 8192 の 1/8。
pub const FFT_LEN: usize = 1024;

/// OFDMサンプルレート [Hz]。フルセグ fIFFT = 512/63 MHz の 1/8。
/// = 8,126,984 / 8 ≈ 1,015,873 Hz
pub const SAMPLE_RATE_HZ: f64 = (512.0e6 / 63.0) / 8.0;

/// 1セグメントの実効帯域 [Hz]（約 429 kHz）。
pub const SEGMENT_BANDWIDTH_HZ: f64 = 428_500.0;

/// キャリア間隔 [Hz] = SAMPLE_RATE / FFT_LEN ≈ 992 Hz（Mode3）。
pub const CARRIER_SPACING_HZ: f64 = SAMPLE_RATE_HZ / FFT_LEN as f64;

/// ガードインターバル比。ISDB-T は 1/4, 1/8, 1/16, 1/32 のいずれか（TMCCで通知）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuardInterval {
    G1_4,
    G1_8,
    G1_16,
    G1_32,
}

impl GuardInterval {
    /// 有効シンボル長 `fft_len` に対するGI（CP）長［サンプル数］。
    pub const fn cp_len(self, fft_len: usize) -> usize {
        match self {
            GuardInterval::G1_4 => fft_len / 4,
            GuardInterval::G1_8 => fft_len / 8,
            GuardInterval::G1_16 => fft_len / 16,
            GuardInterval::G1_32 => fft_len / 32,
        }
    }

    /// 1OFDMシンボル長（有効長＋CP長）。
    pub const fn symbol_len(self, fft_len: usize) -> usize {
        fft_len + self.cp_len(fft_len)
    }

    /// 全GI候補（未知GIの探索用）。
    pub const ALL: [GuardInterval; 4] =
        [Self::G1_4, Self::G1_8, Self::G1_16, Self::G1_32];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cp_lengths() {
        assert_eq!(GuardInterval::G1_4.cp_len(1024), 256);
        assert_eq!(GuardInterval::G1_8.cp_len(1024), 128);
        assert_eq!(GuardInterval::G1_16.cp_len(1024), 64);
        assert_eq!(GuardInterval::G1_32.cp_len(1024), 32);
        assert_eq!(GuardInterval::G1_8.symbol_len(1024), 1152);
    }

    #[test]
    fn carrier_spacing_is_about_992hz() {
        // Mode3 のキャリア間隔は約 0.992 kHz
        assert!((CARRIER_SPACING_HZ - 992.0).abs() < 5.0);
    }
}
