//! ③チャネル等化のためのパイロット定義（SP/PRBS）。
//!
//! ISDB-T のスキャッタードパイロット(SP)は、PRBS系列 `w_i` で BPSK 変調され、
//! 通常データより 4/3 倍にブーストされた既知信号。受信側はこれを使って
//! チャネル `H(f)` を推定する（[`crate::equalize`]）。
//!
//! ## 1セグ（部分受信）でのキャリア配置
//! フルセグ Mode3 のアクティブキャリアは `13*432+1 = 5617` 本。13セグメントは
//! 周波数順に論理セグメント `{11,9,7,5,3,1,0,2,4,6,8,10,12}` の並びで、
//! **論理セグメント0（＝部分受信＝1セグ）は中央（周波数位置6番目）**。
//! よってその 432 本はアクティブキャリア絶対index `2592..3023`
//! （`2592 = 6*432 = 216*12`）を占める。`2592` が12の倍数なので、SPの位相
//! 条件 `i mod 12 == 3*(symbol%4)` はセグメント内ローカルindex `l` でも
//! そのまま `l mod 12 == 3*(symbol%4)` で成立する。
//!
//! 一次資料 ARIB STD-B31／参照実装 gr-isdbt `pilot_signals_impl.cc`。

/// フルセグ Mode3 のアクティブキャリア総数（`13*432 + 1`）。
pub const ACTIVE_CARRIERS_8K: usize = 5617;

/// 1セグメントあたりのキャリア数（Mode3）。
pub const SEGMENT_CARRIERS: usize = 432;

/// 中央セグメント（論理seg0＝1セグ）の先頭アクティブキャリア絶対index。
/// 周波数位置6番目 → `6 * 432`。
pub const CENTER_SEGMENT_OFFSET: usize = 6 * SEGMENT_CARRIERS; // 2592

/// SPのキャリア間隔（12本ごと）。
pub const SP_SPACING: usize = 12;

/// PRBS系列 `w_i` から作るパイロット値（実数 ±4/3）を `active` 本ぶん生成する。
///
/// gr-isdbt / ARIB STD-B31 と同一：11段LFSR、初期値オール1、
/// 帰還 `new = bit2 XOR bit0`（G(x)=X¹¹+X²+1）、出力はLSB。
/// 値は `(4*2*(0.5 - w_i))/3` ＝ `w_i=0 → +4/3`, `w_i=1 → -4/3`。
pub fn prbs_pilot_values(active: usize) -> Vec<f32> {
    let mut reg: u32 = (1 << 11) - 1; // 11個の1
    let mut out = Vec::with_capacity(active);
    for _ in 0..active {
        let w = reg & 0x1; // 出力はLSB
        let new_bit = ((reg >> 2) ^ reg) & 0x1; // bit2 XOR bit0
        reg = (reg >> 1) | (new_bit << 10);
        out.push((4.0 * 2.0 * (0.5 - w as f32)) / 3.0);
    }
    out
}

/// 1セグ（中央セグメント）のパイロット定義。
///
/// `values[l]` は、セグメント内ローカルキャリア `l (0..432)` の
/// PRBSパイロット値（＝絶対index `CENTER_SEGMENT_OFFSET + l` の `w_i`）。
pub struct SegmentPilots {
    /// 中央セグメント432本ぶんの ±4/3 パイロット値。
    pub values: Vec<f32>,
}

impl SegmentPilots {
    /// 中央セグメント（1セグ）のパイロット定義を作る。
    pub fn center_1seg() -> Self {
        let full = prbs_pilot_values(CENTER_SEGMENT_OFFSET + SEGMENT_CARRIERS);
        let values = full[CENTER_SEGMENT_OFFSET..CENTER_SEGMENT_OFFSET + SEGMENT_CARRIERS].to_vec();
        Self { values }
    }

    /// `symbol % 4` のシンボルにおけるSPのローカルキャリアindexを昇順で返す。
    /// 条件：`l mod 12 == 3*(symbol%4)`。
    pub fn sp_carriers(&self, symbol_mod4: usize) -> impl Iterator<Item = usize> {
        let start = 3 * (symbol_mod4 % 4);
        (start..SEGMENT_CARRIERS).step_by(SP_SPACING)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prbs_starts_with_eleven_ones_then_zero() {
        // w0..w10 = 1（パイロット -4/3）, w11 = 0（パイロット +4/3）
        let v = prbs_pilot_values(16);
        for k in 0..11 {
            assert!(
                (v[k] - (-4.0 / 3.0)).abs() < 1e-6,
                "w{k} should be 1 → -4/3"
            );
        }
        assert!((v[11] - (4.0 / 3.0)).abs() < 1e-6, "w11 should be 0 → +4/3");
    }

    #[test]
    fn prbs_is_periodic_with_period_2047() {
        // 11段最大長LFSRの周期は 2^11 - 1 = 2047
        let v = prbs_pilot_values(2047 + 32);
        for k in 0..32 {
            assert_eq!(v[k], v[k + 2047], "PRBS should repeat every 2047 at k={k}");
        }
    }

    #[test]
    fn pilot_values_are_plus_minus_four_thirds() {
        let v = prbs_pilot_values(500);
        for x in v {
            assert!((x.abs() - 4.0 / 3.0).abs() < 1e-6);
        }
    }

    #[test]
    fn center_segment_offset_is_multiple_of_12() {
        // SP位相条件をローカルindexでそのまま使える前提
        assert_eq!(CENTER_SEGMENT_OFFSET % SP_SPACING, 0);
        assert_eq!(CENTER_SEGMENT_OFFSET, 2592);
    }

    #[test]
    fn sp_carriers_layout() {
        let p = SegmentPilots::center_1seg();
        // symbol%4 = 0 → 0,12,24,...,420（36本）
        let sp0: Vec<usize> = p.sp_carriers(0).collect();
        assert_eq!(sp0.first(), Some(&0));
        assert_eq!(sp0.len(), 36);
        assert_eq!(*sp0.last().unwrap(), 420);
        // symbol%4 = 3 → 9,21,...,429
        let sp3: Vec<usize> = p.sp_carriers(3).collect();
        assert_eq!(sp3.first(), Some(&9));
        assert_eq!(*sp3.last().unwrap(), 429);
        // 各SPの値は ±4/3
        for l in sp0 {
            assert!((p.values[l].abs() - 4.0 / 3.0).abs() < 1e-6);
        }
    }
}
