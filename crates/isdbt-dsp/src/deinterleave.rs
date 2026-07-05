//! ④ デインターリーブ（複素キャリア領域の2段）。
//!
//! 受信チェーン（gr-isdbt の接続順で確認）：
//! `②③等化 → TMCC → 周波数デインターリーブ → 時間デインターリーブ → デマップ → …`
//!
//! 1セグ（中央＝論理セグメント0）では：
//! - **周波数デインターリーブ**は、セグメント間処理＝恒等・ローテーション量=0 なので
//!   実質 **384データキャリアのランダム置換 [`FREQ_PERM_MODE3`] だけ**に簡約される。
//! - **時間デインターリーブ**は、データキャリアごとに遅延 `I*(95-mi)` のFIFO遅延線
//!   （`mi=(5c) mod 96`）。本ファイルは gr-isdbt `time_deinterleaver_impl.cc` 準拠。
//!
//! 入力の384データキャリアは [`data_carrier_indices`] が返す順（周波数昇順、SP/TMCC/AC除外）。

use crate::pilots::SegmentPilots;
use crate::tmcc::TMCC_LOCAL_CARRIERS;
use num_complex::Complex32;
use std::collections::VecDeque;

/// 1セグメントのデータキャリア数（Mode3）= 96 × 2^(3-1)。
pub const DATA_CARRIERS: usize = 384;

/// 周波数インターリーブの基準（Mode1のデータキャリア数）。`mi = (5c) mod 96`。
pub const DATA_CARRIERS_MODE1: usize = 96;

/// 中央セグメント内のAC補助キャリア（ローカルindex）。絶対 2599/2681/2798/2801/2818/2836/2969/2999。
pub const AC_LOCAL_CARRIERS: [usize; 8] = [7, 89, 206, 209, 226, 244, 377, 407];

/// Mode3の周波数デインターリーブ・ランダム置換表（ARIB STD-B31 / gr-isdbt）。
/// `out[i] = in[FREQ_PERM_MODE3[i]]`。0..384 の全単射。
pub const FREQ_PERM_MODE3: [u16; DATA_CARRIERS] = [
    62, 13, 371, 11, 285, 336, 365, 220, 226, 92, 56, 46, 120, 175, 298, 352, 172, 235, 53, 164,
    368, 187, 125, 82, 5, 45, 173, 258, 135, 182, 141, 273, 126, 264, 286, 88, 233, 61, 249, 367,
    310, 179, 155, 57, 123, 208, 14, 227, 100, 311, 205, 79, 184, 185, 328, 77, 115, 277, 112, 20,
    199, 178, 143, 152, 215, 204, 139, 234, 358, 192, 309, 183, 81, 129, 256, 314, 101, 43, 97,
    324, 142, 157, 90, 214, 102, 29, 303, 363, 261, 31, 22, 52, 305, 301, 293, 177, 116, 296, 85,
    196, 191, 114, 58, 198, 16, 167, 145, 119, 245, 113, 295, 193, 232, 17, 108, 283, 246, 64, 237,
    189, 128, 373, 302, 320, 239, 335, 356, 39, 347, 351, 73, 158, 276, 243, 99, 38, 287, 3, 330,
    153, 315, 117, 289, 213, 210, 149, 383, 337, 339, 151, 241, 321, 217, 30, 334, 161, 322, 49,
    176, 359, 12, 346, 60, 28, 229, 265, 288, 225, 382, 59, 181, 170, 319, 341, 86, 251, 133, 344,
    361, 109, 44, 369, 268, 257, 323, 55, 317, 381, 121, 360, 260, 275, 190, 19, 63, 18, 248, 9,
    240, 211, 150, 230, 332, 231, 71, 255, 350, 355, 83, 87, 154, 218, 138, 269, 348, 130, 160,
    278, 377, 216, 236, 308, 223, 254, 25, 98, 300, 201, 137, 219, 36, 325, 124, 66, 353, 169, 21,
    35, 107, 50, 106, 333, 326, 262, 252, 271, 263, 372, 136, 0, 366, 206, 159, 122, 188, 6, 284,
    96, 26, 200, 197, 186, 345, 340, 349, 103, 84, 228, 212, 2, 67, 318, 1, 74, 342, 166, 194, 33,
    68, 267, 111, 118, 140, 195, 105, 202, 291, 259, 23, 171, 65, 281, 24, 165, 8, 94, 222, 331,
    34, 238, 364, 376, 266, 89, 80, 253, 163, 280, 247, 4, 362, 379, 290, 279, 54, 78, 180, 72,
    316, 282, 131, 207, 343, 370, 306, 221, 132, 7, 148, 299, 168, 224, 48, 47, 357, 313, 75, 104,
    70, 147, 40, 110, 374, 69, 146, 37, 375, 354, 174, 41, 32, 304, 307, 312, 15, 272, 134, 242,
    203, 209, 380, 162, 297, 327, 10, 93, 42, 250, 156, 338, 292, 144, 378, 294, 329, 127, 270, 76,
    95, 91, 244, 274, 27, 51,
];

/// あるシンボル位相 `symbol%4` のデータキャリア（ローカルindex, 384本, 周波数昇順）。
/// SP（位相依存）・TMCC・AC を除いた残り。
pub fn data_carrier_indices(symbol_mod4: usize, pilots: &SegmentPilots) -> Vec<usize> {
    let mut excluded = [false; 432];
    for l in pilots.sp_carriers(symbol_mod4) {
        excluded[l] = true;
    }
    for &l in &TMCC_LOCAL_CARRIERS {
        excluded[l] = true;
    }
    for &l in &AC_LOCAL_CARRIERS {
        excluded[l] = true;
    }
    (0..432).filter(|&l| !excluded[l]).collect()
}

/// 周波数デインターリーブ（1セグ）：`out[i] = data[FREQ_PERM_MODE3[i]]`。
pub fn freq_deinterleave(data: &[Complex32]) -> Vec<Complex32> {
    assert_eq!(data.len(), DATA_CARRIERS, "データキャリア数が384でない");
    FREQ_PERM_MODE3.iter().map(|&p| data[p as usize]).collect()
}

/// 時間デインターリーブ：キャリアごとのFIFO遅延線。
///
/// キャリア `c` の遅延 = `I*(95 - (5c mod 96))`。全キャリアの遅延合計が一定
/// （`I*95`）になるよう設計されており、フィル後は一定レイテンシで元の時間並びに戻る。
pub struct TimeDeinterleaver {
    bufs: Vec<VecDeque<Complex32>>,
    /// 各キャリアの遅延量（シンボル数）= `I*(95 - mi)`。
    delays: Vec<usize>,
}

impl TimeDeinterleaver {
    /// インターリーブ長 `i`（Layer Aの1セグなら通常4）で初期化。
    pub fn new(i: usize) -> Self {
        let zero = Complex32::new(0.0, 0.0);
        let mut bufs = Vec::with_capacity(DATA_CARRIERS);
        let mut delays = Vec::with_capacity(DATA_CARRIERS);
        for c in 0..DATA_CARRIERS {
            let mi = (5 * c) % DATA_CARRIERS_MODE1;
            // FIFO（push_back→pop_front）では初期ゼロ長がそのまま遅延量になる。
            let delay = i * (DATA_CARRIERS_MODE1 - 1 - mi);
            bufs.push(VecDeque::from(vec![zero; delay]));
            delays.push(delay);
        }
        Self { bufs, delays }
    }

    /// 全キャリアの遅延が揃う総レイテンシ `I*95`（このシンボル数だけ進むと有効出力になる）。
    pub fn latency(&self) -> usize {
        self.delays.iter().copied().max().unwrap_or(0)
    }

    /// 1シンボル（384キャリア）を投入し、各キャリアの遅延線先頭を出力する。
    pub fn push_symbol(&mut self, data: &[Complex32]) -> Vec<Complex32> {
        assert_eq!(data.len(), DATA_CARRIERS, "データキャリア数が384でない");
        let mut out = Vec::with_capacity(DATA_CARRIERS);
        for c in 0..DATA_CARRIERS {
            self.bufs[c].push_back(data[c]);
            out.push(self.bufs[c].pop_front().unwrap());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pilots::SegmentPilots;

    #[test]
    fn freq_perm_is_a_bijection() {
        let mut seen = [false; DATA_CARRIERS];
        for &p in &FREQ_PERM_MODE3 {
            assert!((p as usize) < DATA_CARRIERS);
            assert!(!seen[p as usize], "重複: {p}");
            seen[p as usize] = true;
        }
        assert!(seen.iter().all(|&x| x));
    }

    #[test]
    fn freq_deinterleave_roundtrips_with_inverse() {
        // 逆置換でインターリーブ→デインターリーブが恒等になるか。
        let x: Vec<Complex32> = (0..DATA_CARRIERS)
            .map(|i| Complex32::new(i as f32, -(i as f32)))
            .collect();
        // インターリーブ: rand[perm[i]] = x[i]
        let mut interleaved = vec![Complex32::new(0.0, 0.0); DATA_CARRIERS];
        for (i, &p) in FREQ_PERM_MODE3.iter().enumerate() {
            interleaved[p as usize] = x[i];
        }
        let recovered = freq_deinterleave(&interleaved);
        assert_eq!(recovered, x);
    }

    #[test]
    fn data_carrier_count_and_disjoint() {
        let pilots = SegmentPilots::center_1seg();
        for phase in 0..4 {
            let d = data_carrier_indices(phase, &pilots);
            assert_eq!(d.len(), DATA_CARRIERS, "phase{phase}: 384本でない");
            // 昇順ユニーク
            assert!(d.windows(2).all(|w| w[0] < w[1]));
            // SP/TMCC/AC と素
            let sp: std::collections::HashSet<usize> = pilots.sp_carriers(phase).collect();
            for &c in &d {
                assert!(!sp.contains(&c));
                assert!(!TMCC_LOCAL_CARRIERS.contains(&c));
                assert!(!AC_LOCAL_CARRIERS.contains(&c));
            }
        }
    }

    #[test]
    fn time_interleave_deinterleave_is_constant_latency_identity() {
        // TX側インターリーブ（遅延 I*mi）と RX側デインターリーブ（遅延 I*(95-mi)）を
        // 直列にすると、全キャリアが一定遅延 I*95 で元に戻る。
        let i = 4usize;
        let total_latency = i * (DATA_CARRIERS_MODE1 - 1); // 4*95 = 380
        let zero = Complex32::new(0.0, 0.0);

        // TX遅延線
        let mut tx: Vec<VecDeque<Complex32>> = (0..DATA_CARRIERS)
            .map(|c| {
                let mi = (5 * c) % DATA_CARRIERS_MODE1;
                VecDeque::from(vec![zero; i * mi]) // TX遅延 I*mi（RXの I*(95-mi) と合わせて I*95）
            })
            .collect();
        let mut rx = TimeDeinterleaver::new(i);

        let nsym = total_latency + 50;
        // 各シンボル・各キャリアに一意な値を入れて、出力が total_latency 遅れの入力と一致するか
        let val = |t: usize, c: usize| Complex32::new((t * 1000 + c) as f32, 0.0);
        let mut inputs: Vec<Vec<Complex32>> = Vec::new();
        for t in 0..nsym {
            let sym: Vec<Complex32> = (0..DATA_CARRIERS).map(|c| val(t, c)).collect();
            inputs.push(sym.clone());
            // TX interleave
            let mut txout = vec![zero; DATA_CARRIERS];
            for c in 0..DATA_CARRIERS {
                tx[c].push_back(sym[c]);
                txout[c] = tx[c].pop_front().unwrap();
            }
            // RX deinterleave
            let out = rx.push_symbol(&txout);
            if t >= total_latency {
                let want = &inputs[t - total_latency];
                assert_eq!(out, *want, "t={t}: 一定遅延の恒等が崩れた");
            }
        }
        assert_eq!(rx.latency(), total_latency);
    }
}
