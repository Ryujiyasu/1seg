//! ④の入口：TMCC（Transmission and Multiplexing Configuration Control）復号。
//!
//! TMCCは伝送パラメータ（各階層の変調方式・畳み込み符号化率・時間インターリーブ長・
//! セグメント数など）を運ぶ制御チャネル。④デマップ／デインターリーブ・⑤FECは
//! これを読まないと始まらない。
//!
//! ## 構造（ARIB STD-B31／参照 gr-isdbt `tmcc_decoder_impl.cc`）
//! - TMCCキャリアは**差動BPSK（DBPSK）**で、シンボル間の位相差で1bit運ぶ：
//!   `Re(X_k · conj(X_{k-1})) ≥ 0 → bit0`（同相）, `< 0 → bit1`（180°反転）。
//! - 同一TMCC語を複数キャリアが冗長伝送 → **多数決**。
//! - **1フレーム = 204 OFDMシンボル = 204 bit**。先頭 `B0` は差動基準、`B1..B16` は
//!   16bit同期語（フレームごとに even/odd で反転）、`B20..` がBCH保護された情報部。
//! - 1セグ（部分受信）は **Layer A**。中央セグメント内のTMCCキャリアは絶対index
//!   2693/2723/2878/2941 → ローカル(=−2592) **101/131/286/349** の4本。

use crate::pilots::CENTER_SEGMENT_OFFSET;
use num_complex::Complex32;

/// 中央セグメント内のTMCCキャリア（ローカルindex）。絶対 2693/2723/2878/2941。
pub const TMCC_LOCAL_CARRIERS: [usize; 4] = [
    2693 - CENTER_SEGMENT_OFFSET,
    2723 - CENTER_SEGMENT_OFFSET,
    2878 - CENTER_SEGMENT_OFFSET,
    2941 - CENTER_SEGMENT_OFFSET,
];

/// 1フレームのOFDMシンボル数（＝TMCCビット数）。
pub const SYMBOLS_PER_FRAME: usize = 204;

/// 同期語長（bit）。
pub const SYNC_SIZE: usize = 16;

/// 偶数フレームの同期語 `B1..B16`。
pub const SYNC_EVEN: [u8; SYNC_SIZE] = [0, 0, 1, 1, 0, 1, 0, 1, 1, 1, 1, 0, 1, 1, 1, 0];
/// 奇数フレームの同期語（even のビット反転）。
pub const SYNC_ODD: [u8; SYNC_SIZE] = [1, 1, 0, 0, 1, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 1];

/// 変調方式（TMCC 3bit）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modulation {
    Dqpsk,
    Qpsk,
    Qam16,
    Qam64,
    Unused,
    Reserved(u8),
}
impl Modulation {
    pub fn from_bits(v: u8) -> Self {
        match v {
            0 => Self::Dqpsk,
            1 => Self::Qpsk,
            2 => Self::Qam16,
            3 => Self::Qam64,
            7 => Self::Unused,
            x => Self::Reserved(x),
        }
    }
}

/// 畳み込み符号化率（TMCC 3bit）を "1/2" 等の文字列で返す。
pub fn coding_rate_str(v: u8) -> &'static str {
    match v {
        0 => "1/2",
        1 => "2/3",
        2 => "3/4",
        3 => "5/6",
        4 => "7/8",
        7 => "未使用",
        _ => "予約",
    }
}

/// 時間インターリーブ長 I（Mode3での値）。`None` は未使用。
pub fn interleaving_mode3(v: u8) -> Option<u8> {
    match v {
        0 => Some(0),
        1 => Some(1),
        2 => Some(2),
        3 => Some(4),
        _ => None,
    }
}

/// 1階層ぶんのTMCC情報。
#[derive(Clone, Copy, Debug)]
pub struct LayerInfo {
    pub modulation: Modulation,
    /// 符号化率の生3bit（文字列は [`coding_rate_str`]）。
    pub coding_rate: u8,
    /// 時間インターリーブの生3bit（Mode3実値は [`interleaving_mode3`]）。
    pub interleaving: u8,
    /// セグメント数（1..13、15=未使用）。
    pub n_segments: u8,
}

/// パースしたTMCC（情報部の主要フィールド）。
#[derive(Clone, Copy, Debug)]
pub struct TmccInfo {
    pub system_id: u8,           // B20..21
    pub switching_indicator: u8, // B22..25
    pub emergency_flag: u8,      // B26
    pub partial_reception: u8,   // B27（1=部分受信あり）
    pub layer_a: LayerInfo,      // B28..40（1セグ）
    pub layer_b: LayerInfo,      // B41..53
    pub layer_c: LayerInfo,      // B54..66
}

fn bits_be(frame: &[u8], start: usize, n: usize) -> u8 {
    let mut v = 0u8;
    for i in 0..n {
        v = (v << 1) | (frame[start + i] & 1);
    }
    v
}

fn layer_at(frame: &[u8], base: usize) -> LayerInfo {
    LayerInfo {
        modulation: Modulation::from_bits(bits_be(frame, base, 3)),
        coding_rate: bits_be(frame, base + 3, 3),
        interleaving: bits_be(frame, base + 6, 3),
        n_segments: bits_be(frame, base + 9, 4),
    }
}

/// 204bitのフレーム（`frame[0]=B0`）からTMCC情報をパースする。
pub fn parse_tmcc(frame: &[u8]) -> TmccInfo {
    TmccInfo {
        system_id: bits_be(frame, 20, 2),
        switching_indicator: bits_be(frame, 22, 4),
        emergency_flag: frame[26] & 1,
        partial_reception: frame[27] & 1,
        layer_a: layer_at(frame, 28),
        layer_b: layer_at(frame, 41),
        layer_c: layer_at(frame, 54),
    }
}

/// 各OFDMシンボルのセグメントスペクトル列から、TMCCのDBPSKビット列を復号する。
///
/// `segments[k]` は長さ432の中央セグメント（[`crate::equalize::extract_segment`] の出力）。
/// 返り値 `bits[i]` は シンボル `i`→`i+1` の遷移で得たビット（長さ `segments.len()-1`）。
/// 4本のTMCCキャリアで多数決する。
pub fn dbpsk_bits(segments: &[Vec<Complex32>]) -> Vec<u8> {
    let mut bits = Vec::with_capacity(segments.len().saturating_sub(1));
    for k in 1..segments.len() {
        let mut acc = 0.0f32;
        for &c in &TMCC_LOCAL_CARRIERS {
            let d = segments[k][c] * segments[k - 1][c].conj();
            acc += d.re;
        }
        bits.push(u8::from(acc < 0.0));
    }
    bits
}

/// フレーム同期の結果。
#[derive(Clone, Debug)]
pub struct FrameSync {
    /// ビット列中で `B0` に当たる位置。フレームは `phase + 204*f` ごと。
    pub phase: usize,
    /// 評価に使えたフレーム数。
    pub n_frames: usize,
    /// 全フレーム合計での同期語一致ビット数 / 総ビット数。
    pub matched: usize,
    pub total: usize,
    /// 各フレームで even(false)/odd(true) どちらに寄ったか。
    pub parity_per_frame: Vec<bool>,
    /// even/odd が1フレームごとに交互だったか（強い整合チェック）。
    pub alternates: bool,
}

fn match_count(w: &[u8], pat: &[u8; SYNC_SIZE]) -> usize {
    w.iter().zip(pat).filter(|(a, b)| *a == *b).count()
}

/// 204通りのフレーム位相を総当たりし、同期語一致が最大の位相を返す。
pub fn find_frame_sync(bits: &[u8]) -> Option<FrameSync> {
    if bits.len() < SYMBOLS_PER_FRAME {
        return None;
    }
    let mut best: Option<FrameSync> = None;
    for phase in 0..SYMBOLS_PER_FRAME {
        let mut matched = 0usize;
        let mut total = 0usize;
        let mut parity = Vec::new();
        let mut f = 0usize;
        loop {
            let start = phase + f * SYMBOLS_PER_FRAME;
            if start + 1 + SYNC_SIZE > bits.len() {
                break;
            }
            let w = &bits[start + 1..start + 1 + SYNC_SIZE];
            let me = match_count(w, &SYNC_EVEN);
            let mo = match_count(w, &SYNC_ODD);
            if me >= mo {
                matched += me;
                parity.push(false);
            } else {
                matched += mo;
                parity.push(true);
            }
            total += SYNC_SIZE;
            f += 1;
        }
        if total == 0 {
            continue;
        }
        let alternates = parity.windows(2).all(|w| w[0] != w[1]) && parity.len() >= 2;
        let cand = FrameSync {
            phase,
            n_frames: f,
            matched,
            total,
            parity_per_frame: parity,
            alternates,
        };
        let better = match &best {
            None => true,
            Some(b) => cand.matched * b.total > b.matched * cand.total,
        };
        if better {
            best = Some(cand);
        }
    }
    best
}

/// `phase` で揃えたあと、各ビット位置をフレーム間で多数決して1フレーム(204bit)に統合する。
/// 情報部はフレーム間で一定なのでSNRが稼げる（同期語B1..16だけは交互なので無視してよい）。
pub fn majority_frame(bits: &[u8], phase: usize) -> Vec<u8> {
    let mut frame = vec![0u8; SYMBOLS_PER_FRAME];
    for (pos, slot) in frame.iter_mut().enumerate() {
        let mut ones = 0i32;
        let mut n = 0i32;
        let mut f = 0usize;
        loop {
            let idx = phase + f * SYMBOLS_PER_FRAME + pos;
            if idx >= bits.len() {
                break;
            }
            if bits[idx] == 1 {
                ones += 1;
            }
            n += 1;
            f += 1;
        }
        *slot = u8::from(n > 0 && ones * 2 >= n);
    }
    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmcc_local_carriers_are_in_segment() {
        assert_eq!(TMCC_LOCAL_CARRIERS, [101, 131, 286, 349]);
        for c in TMCC_LOCAL_CARRIERS {
            assert!(c < 432);
        }
    }

    #[test]
    fn sync_odd_is_complement_of_even() {
        for i in 0..SYNC_SIZE {
            assert_eq!(SYNC_EVEN[i] ^ SYNC_ODD[i], 1);
        }
    }

    /// 既知TMCCフレームをDBPSK変調 → 復号 → 同期 → パースの往復。
    #[test]
    fn dbpsk_roundtrip_sync_and_parse() {
        // 既知の情報部を持つ1フレーム(204bit)を作る（B0=0基準, sync, 情報部）。
        let mut frame = vec![0u8; SYMBOLS_PER_FRAME];
        // B1..16 = even sync
        frame[1..1 + SYNC_SIZE].copy_from_slice(&SYNC_EVEN);
        // 情報部：partial=1, Layer A = QPSK(1)/CR=2_3(1)/IL=3(→Mode3 I=4)/SEG=1
        frame[27] = 1; // partial reception
                       // Layer A: mod[28..30]=001(QPSK), cr[31..33]=001(2/3), il[34..36]=011(I=4), seg[37..40]=0001
        let la = [
            (28, 0),
            (29, 0),
            (30, 1),
            (31, 0),
            (32, 0),
            (33, 1),
            (34, 0),
            (35, 1),
            (36, 1),
            (37, 0),
            (38, 0),
            (39, 0),
            (40, 1),
        ];
        for (i, v) in la {
            frame[i] = v;
        }

        // 2.x フレームぶんのビット列を作る（同期語は偶奇交互、情報部は同一）。
        let n_frames = 3;
        let mut stream: Vec<u8> = Vec::new();
        for f in 0..n_frames {
            let mut fr = frame.clone();
            if f % 2 == 1 {
                fr[1..1 + SYNC_SIZE].copy_from_slice(&SYNC_ODD);
            }
            stream.extend_from_slice(&fr);
        }
        // 先頭にズレ（位相）を足す
        let lead = 37usize;
        let mut bits_stream = vec![0u8; lead];
        bits_stream.extend_from_slice(&stream);

        // DBPSKで4本のTMCCキャリアに変調した合成セグメント列を作る。
        // bit列 b[i] は seg[i]→seg[i+1] の遷移。seg数 = bits+1。
        let nsym = bits_stream.len() + 1;
        let mut segs: Vec<Vec<Complex32>> = vec![vec![Complex32::new(0.3, -0.1); 432]; nsym];
        // 各TMCCキャリアの位相を差動で進める
        for &c in &TMCC_LOCAL_CARRIERS {
            let mut ph = Complex32::new(1.0, 0.0);
            segs[0][c] = ph;
            for (i, &b) in bits_stream.iter().enumerate() {
                if b == 1 {
                    ph = -ph; // bit1で反転
                }
                segs[i + 1][c] = ph;
            }
        }

        let decoded = dbpsk_bits(&segs);
        assert_eq!(decoded, bits_stream, "DBPSK往復が一致しない");

        let fs = find_frame_sync(&decoded).expect("同期できない");
        assert_eq!(fs.phase, lead, "フレーム位相がズレ");
        assert_eq!(fs.matched, fs.total, "同期語が完全一致でない");
        assert!(fs.alternates, "even/oddが交互でない");

        let mf = majority_frame(&decoded, fs.phase);
        let info = parse_tmcc(&mf);
        assert_eq!(info.partial_reception, 1);
        assert_eq!(info.layer_a.modulation, Modulation::Qpsk);
        assert_eq!(coding_rate_str(info.layer_a.coding_rate), "2/3");
        assert_eq!(interleaving_mode3(info.layer_a.interleaving), Some(4));
        assert_eq!(info.layer_a.n_segments, 1);
    }
}
