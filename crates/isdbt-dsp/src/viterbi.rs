//! ⑤ FEC 内符号：畳み込み符号のViterbi復号（depuncture込み）。
//!
//! ISDB-T/DVB-T の内符号は **K=7・母符号レート1/2**、生成多項式
//! `G1 = 0o171`, `G2 = 0o133`（出力順 X=G1, Y=G2）。これを 2/3, 3/4, 5/6, 7/8 に
//! パンクチャして送る。Layer A（1セグ）は通常 **2/3**。
//!
//! 本実装は**ソフト入力**（各符号ビットを `f32`：`+`寄り→1, `-`寄り→0, `0.0`→消失）。
//! depuncture で抜かれたビットは `0.0`（消失）として入れれば分岐メトリクスに寄与しない。
//! 分岐メトリクスは相関（候補出力を±1にして受信ソフト値と内積）を**最大化**する。
//!
//! gr-isdbt の Viterbi は SSE intrinsic 版（多項式はビット反転 0x4f/0x6d）だが、
//! ここでは標準多項式で素直な全トレースバックを自前実装している。

/// 拘束長 K。状態数は `2^(K-1) = 64`。
pub const K: usize = 7;
/// 状態数。
pub const N_STATES: usize = 1 << (K - 1);
/// 生成多項式 G1 = 0o171（出力X）。
pub const G1: u16 = 0o171;
/// 生成多項式 G2 = 0o133（出力Y）。
pub const G2: u16 = 0o133;

/// レート2/3 のパンクチャパターン（母符号の X0,Y0,X1,Y1 のうち X1 を抜く）。
pub const PUNCTURE_2_3: [u8; 4] = [1, 1, 0, 1];
/// レート3/4。
pub const PUNCTURE_3_4: [u8; 6] = [1, 1, 0, 1, 1, 0];
/// レート1/2（無パンクチャ）。
pub const PUNCTURE_1_2: [u8; 2] = [1, 1];

fn parity(mut x: u16) -> u8 {
    x ^= x >> 8;
    x ^= x >> 4;
    x ^= x >> 2;
    x ^= x >> 1;
    (x & 1) as u8
}

/// 状態 `s`（直近6入力, 最新がbit5）に入力 `u` を入れたときの
/// (出力X, 出力Y, 次状態)。
#[inline]
fn step(s: usize, u: u8) -> (u8, u8, usize) {
    let reg = ((u as u16) << 6) | (s as u16); // 7bit: bit6=今回入力, bit5..0=状態
    let x = parity(reg & G1);
    let y = parity(reg & G2);
    let ns = ((reg >> 1) & 0x3F) as usize; // 直近6入力（今回入力がbit5へ）
    (x, y, ns)
}

/// K=7・レート1/2 の畳み込み符号化（検証・送信側用）。
/// 末尾に K-1=6 個の0を足して状態0で終端する。返り値は2ビット/入力（X,Y,X,Y,…）。
pub fn conv_encode_terminated(info: &[u8]) -> Vec<u8> {
    let mut s = 0usize;
    let mut out = Vec::with_capacity((info.len() + K - 1) * 2);
    let flush = [0u8; K - 1];
    for u in info.iter().copied().chain(flush) {
        let (x, y, ns) = step(s, u & 1);
        out.push(x);
        out.push(y);
        s = ns;
    }
    out
}

/// K=7・レート1/2 の畳み込み符号化（終端なし・連続）。再エンコード照合用。
/// 始端状態 `s0` から符号化し、(符号ビット列, 終了状態) を返す。
pub fn conv_encode(info: &[u8], s0: usize) -> (Vec<u8>, usize) {
    let mut s = s0 & 0x3F;
    let mut out = Vec::with_capacity(info.len() * 2);
    for &u in info {
        let (x, y, ns) = step(s, u & 1);
        out.push(x);
        out.push(y);
        s = ns;
    }
    (out, s)
}

/// パンクチャ：母符号ビット列から、パターンが1の位置だけ残す。
pub fn puncture(mother: &[u8], pattern: &[u8]) -> Vec<u8> {
    mother
        .iter()
        .enumerate()
        .filter(|(i, _)| pattern[i % pattern.len()] == 1)
        .map(|(_, &b)| b)
        .collect()
}

/// デパンクチャ：受信ソフト列を母符号位置へ戻し、抜かれた位置に `0.0`（消失）を挿入する。
/// `rx` は「パターン1の位置」のソフト値の並び。返り値は母符号長のソフト列。
pub fn depuncture(rx: &[f32], pattern: &[u8]) -> Vec<f32> {
    let kept = pattern.iter().filter(|&&p| p == 1).count();
    let groups = rx.len() / kept;
    let mut out = Vec::with_capacity(groups * pattern.len());
    let mut it = rx.iter();
    for _ in 0..groups {
        for &p in pattern {
            if p == 1 {
                out.push(*it.next().unwrap());
            } else {
                out.push(0.0); // 消失
            }
        }
    }
    out
}

/// ハードビット列をソフト値へ：`1 → +1.0`, `0 → -1.0`。
pub fn hard_to_soft(bits: &[u8]) -> Vec<f32> {
    bits.iter()
        .map(|&b| if b & 1 == 1 { 1.0 } else { -1.0 })
        .collect()
}

/// 全トレースバックのソフト判定Viterbi復号器（K=7・母レート1/2）。
pub struct Viterbi {
    /// (state, u) → (out_x, out_y, next_state) を事前計算。
    table: Vec<[(f32, f32, usize); 2]>,
}

impl Default for Viterbi {
    fn default() -> Self {
        Self::new()
    }
}

impl Viterbi {
    pub fn new() -> Self {
        let mut table = vec![[(0.0, 0.0, 0usize); 2]; N_STATES];
        for s in 0..N_STATES {
            for u in 0..2u8 {
                let (x, y, ns) = step(s, u);
                // 出力を ±1 に（1→+1, 0→-1）して相関メトリクスに使う
                let sx = if x == 1 { 1.0 } else { -1.0 };
                let sy = if y == 1 { 1.0 } else { -1.0 };
                table[s][u as usize] = (sx, sy, ns);
            }
        }
        Self { table }
    }

    /// 母符号ソフト列 `coded`（長さは偶数、2値で1トレリスステップ）を復号する。
    /// 始端は状態0と仮定（終端は最良状態でトレースバック）。
    /// 返り値は推定情報ビット列（ステップ数 = coded.len()/2）。
    pub fn decode(&self, coded: &[f32]) -> Vec<u8> {
        let steps = coded.len() / 2;
        if steps == 0 {
            return Vec::new();
        }
        const NEG_INF: f32 = f32::NEG_INFINITY;
        let mut metric = vec![NEG_INF; N_STATES];
        metric[0] = 0.0; // 始端は状態0
                         // 各ステップ・各状態の生存元入力ビット（トレースバック用）
        let mut back = vec![0u8; steps * N_STATES];
        let mut prev = vec![0usize; steps * N_STATES];

        let mut next = vec![NEG_INF; N_STATES];
        for t in 0..steps {
            let r1 = coded[2 * t];
            let r2 = coded[2 * t + 1];
            for m in next.iter_mut() {
                *m = NEG_INF;
            }
            for s in 0..N_STATES {
                if metric[s] == NEG_INF {
                    continue;
                }
                for u in 0..2usize {
                    let (sx, sy, ns) = self.table[s][u];
                    let bm = r1 * sx + r2 * sy; // 相関（大きいほど一致）
                    let cand = metric[s] + bm;
                    if cand > next[ns] {
                        next[ns] = cand;
                        back[t * N_STATES + ns] = u as u8;
                        prev[t * N_STATES + ns] = s;
                    }
                }
            }
            std::mem::swap(&mut metric, &mut next);
        }

        // 終端：最良の最終状態からトレースバック（NaN は Equal 扱いで unwrap 回避）
        let mut s = (0..N_STATES)
            .max_by(|&a, &b| {
                metric[a]
                    .partial_cmp(&metric[b])
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap();
        let mut bits = vec![0u8; steps];
        for t in (0..steps).rev() {
            bits[t] = back[t * N_STATES + s];
            s = prev[t * N_STATES + s];
        }
        bits
    }
}

/// ストリーミング用ソフト判定Viterbi（固定トレースバック長）。リアルタイム復調向け。
/// 母符号ソフト対を1つずつ食わせると、`depth` ステップ遅れて情報ビットが1つ出る。
pub struct ViterbiStreaming {
    table: Vec<[(f32, f32, usize); 2]>,
    metric: Vec<f32>,
    next: Vec<f32>, // 再利用スクラッチ（毎ステップの確保を回避）
    depth: usize,
    /// リングバッファ（各 `depth` スロットに 各状態の生存元・入力ビット）。
    prev: Vec<Vec<u8>>,
    back: Vec<Vec<u8>>,
    n: usize, // 処理済みステップ数
}

impl ViterbiStreaming {
    /// トレースバック長 `depth`（K=7 なら 5K〜=35以上、実用は 96 程度）で作る。
    pub fn new(depth: usize) -> Self {
        let mut table = vec![[(0.0, 0.0, 0usize); 2]; N_STATES];
        for s in 0..N_STATES {
            for u in 0..2u8 {
                let (x, y, ns) = step(s, u);
                let sx = if x == 1 { 1.0 } else { -1.0 };
                let sy = if y == 1 { 1.0 } else { -1.0 };
                table[s][u as usize] = (sx, sy, ns);
            }
        }
        let mut metric = vec![f32::NEG_INFINITY; N_STATES];
        metric[0] = 0.0;
        Self {
            table,
            metric,
            next: vec![f32::NEG_INFINITY; N_STATES],
            depth,
            prev: vec![vec![0u8; N_STATES]; depth],
            back: vec![vec![0u8; N_STATES]; depth],
            n: 0,
        }
    }

    /// 母符号ソフト対 `(r1, r2)` を1ステップ投入。`depth` 充填後は情報ビットを1つ返す。
    pub fn push(&mut self, r1: f32, r2: f32) -> Option<u8> {
        let slot = self.n % self.depth;
        for m in self.next.iter_mut() {
            *m = f32::NEG_INFINITY;
        }
        let prev_slot = &mut self.prev[slot];
        let back_slot = &mut self.back[slot];
        for s in 0..N_STATES {
            let ms = self.metric[s];
            if ms == f32::NEG_INFINITY {
                continue;
            }
            for u in 0..2usize {
                let (sx, sy, ns) = self.table[s][u];
                let cand = ms + r1 * sx + r2 * sy;
                if cand > self.next[ns] {
                    self.next[ns] = cand;
                    prev_slot[ns] = s as u8;
                    back_slot[ns] = u as u8;
                }
            }
        }
        // メトリクス正規化（最大を引いてオーバーフロー防止）
        let mx = self.next.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        if mx.is_finite() {
            for m in self.next.iter_mut() {
                if m.is_finite() {
                    *m -= mx;
                }
            }
        }
        std::mem::swap(&mut self.metric, &mut self.next); // 確保なしで入れ替え
        self.n += 1;

        if self.n < self.depth {
            return None;
        }
        // 現在の最良状態から depth 遡り、最古ステップの入力ビットを出す
        let mut s = (0..N_STATES)
            .max_by(|&a, &b| {
                self.metric[a]
                    .partial_cmp(&self.metric[b])
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap() as u8;
        let mut out = 0u8;
        for i in 0..self.depth {
            let step_idx = self.n - 1 - i;
            let sl = step_idx % self.depth;
            out = self.back[sl][s as usize];
            s = self.prev[sl][s as usize];
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(seed: &mut u32) -> u32 {
        *seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        *seed
    }

    #[test]
    fn parity_basic() {
        assert_eq!(parity(0b0), 0);
        assert_eq!(parity(0b1), 1);
        assert_eq!(parity(0b11), 0);
        assert_eq!(parity(0o171), 1); // 1111001 → 5 ones → odd
    }

    #[test]
    fn rate_1_2_roundtrip_noiseless() {
        let mut seed = 12345u32;
        let info: Vec<u8> = (0..500).map(|_| (lcg(&mut seed) >> 16 & 1) as u8).collect();
        let coded = conv_encode_terminated(&info);
        let soft = hard_to_soft(&coded);
        let v = Viterbi::new();
        let dec = v.decode(&soft);
        // 復号は情報ビット＋終端6bit。先頭 info.len() が一致するはず。
        assert_eq!(&dec[..info.len()], &info[..], "rate1/2 無雑音で不一致");
    }

    #[test]
    fn rate_2_3_roundtrip_noiseless() {
        let mut seed = 999u32;
        let info: Vec<u8> = (0..600).map(|_| (lcg(&mut seed) >> 16 & 1) as u8).collect();
        let coded = conv_encode_terminated(&info);
        let punctured = puncture(&coded, &PUNCTURE_2_3);
        let soft = hard_to_soft(&punctured);
        let restored = depuncture(&soft, &PUNCTURE_2_3);
        let v = Viterbi::new();
        let dec = v.decode(&restored);
        assert_eq!(&dec[..info.len()], &info[..], "rate2/3 無雑音で不一致");
    }

    #[test]
    fn streaming_matches_batch_with_latency() {
        let mut seed = 555u32;
        let info: Vec<u8> = (0..800).map(|_| (lcg(&mut seed) >> 16 & 1) as u8).collect();
        let coded = conv_encode_terminated(&info);
        let soft = hard_to_soft(&coded);
        let depth = 96usize;
        let mut vs = ViterbiStreaming::new(depth);
        let mut out = Vec::new();
        for t in 0..soft.len() / 2 {
            if let Some(b) = vs.push(soft[2 * t], soft[2 * t + 1]) {
                out.push(b);
            }
        }
        // ストリーミング出力は depth 遅れ。out[i] == info[i]（先頭 info.len()-depth まで）
        let n = info.len().saturating_sub(depth);
        assert_eq!(
            &out[..n],
            &info[..n],
            "ストリーミングViterbiがバッチと不一致"
        );
    }

    #[test]
    fn corrects_random_bit_errors_rate_1_2() {
        let mut seed = 7u32;
        let info: Vec<u8> = (0..1000)
            .map(|_| (lcg(&mut seed) >> 16 & 1) as u8)
            .collect();
        let coded = conv_encode_terminated(&info);
        let mut soft = hard_to_soft(&coded);
        // 約3%の符号ビットを反転（K=7・1/2 の訂正能力内）
        let nflip = soft.len() * 3 / 100;
        for _ in 0..nflip {
            let idx = (lcg(&mut seed) as usize) % soft.len();
            soft[idx] = -soft[idx];
        }
        let v = Viterbi::new();
        let dec = v.decode(&soft);
        assert_eq!(&dec[..info.len()], &info[..], "3%誤りを訂正できていない");
    }
}
