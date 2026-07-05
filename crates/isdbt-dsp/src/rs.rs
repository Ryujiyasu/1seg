//! ⑤外符号：RS(204,188) 短縮リードソロモン（GF(256), 0x11d, fcr=0, prim=1, 16 parity）。
//!
//! MPEG-2/ISDB-T の外符号。RS(255,239) を 51バイト0詰めで短縮して (204,188) にする。
//! ここではシンドローム計算（＝正しいバイトデインタ整列の検証にも使える）と、
//! Berlekamp-Massey による誤り訂正復号を実装する。
//! 参照：Phil Karn RS / gr-isdbt `reed_solomon_dec_isdbt_impl.cc`（gfpoly=0x11d, n=255,k=239）。

/// 符号語長（短縮後）。
pub const N: usize = 204;
/// 情報長。
pub const K: usize = 188;
/// パリティ数。
pub const NROOTS: usize = N - K; // 16
const GF_POLY: u16 = 0x11d;

struct Gf {
    exp: [u8; 512],
    log: [u8; 256],
}

fn build_gf() -> Gf {
    let mut exp = [0u8; 512];
    let mut log = [0u8; 256];
    let mut x = 1u16;
    for i in 0..255 {
        exp[i] = x as u8;
        log[x as usize] = i as u8;
        x <<= 1;
        if x & 0x100 != 0 {
            x ^= GF_POLY;
        }
    }
    for i in 255..512 {
        exp[i] = exp[i - 255];
    }
    Gf { exp, log }
}

impl Gf {
    #[inline]
    fn mul(&self, a: u8, b: u8) -> u8 {
        if a == 0 || b == 0 {
            0
        } else {
            self.exp[self.log[a as usize] as usize + self.log[b as usize] as usize]
        }
    }
    #[inline]
    fn inv(&self, a: u8) -> u8 {
        self.exp[255 - self.log[a as usize] as usize]
    }
    /// alpha^p。
    #[inline]
    fn pow(&self, p: usize) -> u8 {
        self.exp[p % 255]
    }
}

/// 204バイト符号語のシンドローム16個を返す。全0なら有効なRS符号語。
/// `block[0]` を最高次係数として Horner 評価（短縮の先頭0詰めは値に影響しない）。
pub fn syndromes(block: &[u8]) -> [u8; NROOTS] {
    let gf = build_gf();
    let mut s = [0u8; NROOTS];
    for (j, sj) in s.iter_mut().enumerate() {
        let root = gf.pow(j); // fcr=0, prim=1 → alpha^j
        let mut acc = 0u8;
        for &b in block {
            acc = gf.mul(acc, root) ^ b;
        }
        *sj = acc;
    }
    s
}

/// シンドローム全0か（＝訂正不要の有効符号語）。
pub fn is_codeword(block: &[u8]) -> bool {
    syndromes(block).iter().all(|&x| x == 0)
}

/// 規約検証用：fcr（最初の連続根の指数）を変えたシンドロームの非0数。
pub fn nonzero_syndromes_fcr(block: &[u8], fcr: usize) -> usize {
    let gf = build_gf();
    let mut nz = 0;
    for j in 0..NROOTS {
        let root = gf.pow(fcr + j);
        let mut acc = 0u8;
        for &b in block {
            acc = gf.mul(acc, root) ^ b;
        }
        if acc != 0 {
            nz += 1;
        }
    }
    nz
}

/// GFテーブルをキャッシュして、多数ブロックを高速にシンドローム検査する。
pub struct Checker {
    gf: Gf,
}
impl Default for Checker {
    fn default() -> Self {
        Self::new()
    }
}
impl Checker {
    pub fn new() -> Self {
        Self { gf: build_gf() }
    }
    /// このブロックが有効なRS符号語（シンドローム全0）か。
    pub fn is_codeword(&self, block: &[u8]) -> bool {
        for j in 0..NROOTS {
            let root = self.gf.pow(j);
            let mut acc = 0u8;
            for &b in block {
                acc = self.gf.mul(acc, root) ^ b;
            }
            if acc != 0 {
                return false;
            }
        }
        true
    }
}

/// RS復号（Berlekamp-Massey + Chien + Forney）。
/// `block` は204バイト。訂正して返す（`Some`）。訂正不能なら `None`。
/// 位置は `block[0]` が最高次（degree N-1）。
pub fn decode(block: &[u8]) -> Option<Vec<u8>> {
    let gf = build_gf();
    let synd = syndromes(block);
    if synd.iter().all(|&x| x == 0) {
        return Some(block.to_vec());
    }

    // Berlekamp-Massey: 誤り位置多項式 sigma を求める
    let mut sigma = vec![1u8];
    let mut b = vec![1u8];
    let mut l = 0usize;
    let mut m = 1usize;
    let mut bb = 1u8;
    for n in 0..NROOTS {
        // discrepancy
        let mut delta = synd[n];
        for i in 1..=l {
            delta ^= gf.mul(sigma[i], synd[n - i]);
        }
        if delta == 0 {
            m += 1;
        } else if 2 * l <= n {
            let t = sigma.clone();
            // sigma = sigma - (delta/bb) x^m b
            let coef = gf.mul(delta, gf.inv(bb));
            let mut scaled = vec![0u8; m];
            scaled.extend(b.iter().map(|&x| gf.mul(coef, x)));
            sigma = poly_sub(&sigma, &scaled);
            l = n + 1 - l;
            b = t;
            bb = delta;
            m = 1;
        } else {
            let coef = gf.mul(delta, gf.inv(bb));
            let mut scaled = vec![0u8; m];
            scaled.extend(b.iter().map(|&x| gf.mul(coef, x)));
            sigma = poly_sub(&sigma, &scaled);
            m += 1;
        }
    }

    let nerr = l;
    if nerr == 0 || nerr > NROOTS / 2 {
        return None;
    }

    // Chien search: sigma の根 → 誤り位置。位置 i（0..N-1, block[0]が最高次 → 位置 i の符号 alpha^-i）
    // 有効符号語長は255短縮204。エラー位置 exponent は (N-1-idx) を使う。
    let mut err_pos = Vec::new();
    for idx in 0..N {
        // 評価点 X^{-1} = alpha^{-(N-1-idx)}
        let xinv = gf.pow((255 - ((N - 1 - idx) % 255)) % 255);
        let mut v = 0u8;
        for (p, &c) in sigma.iter().enumerate() {
            v ^= gf.mul(c, gf.pow((gf.log(xinv) as usize) * p));
        }
        if v == 0 {
            err_pos.push(idx);
        }
    }
    if err_pos.len() != nerr {
        return None;
    }

    // Forney: 誤り値。omega = (sigma*S) mod x^NROOTS
    let mut synd_poly = vec![0u8; NROOTS];
    for j in 0..NROOTS {
        synd_poly[j] = synd[j];
    }
    let omega = poly_mul_mod(&gf, &sigma, &synd_poly, NROOTS);
    // sigma'（形式微分）
    let mut out = block.to_vec();
    for &idx in &err_pos {
        let exp_pos = (N - 1 - idx) % 255;
        let x = gf.pow(exp_pos); // X = alpha^{pos}
        let xinv = gf.inv(x);
        // omega(X^-1)
        let mut num = 0u8;
        for (p, &c) in omega.iter().enumerate() {
            num ^= gf.mul(c, gf.pow((gf.log(xinv) as usize) * p));
        }
        // sigma'(X^-1)
        let mut den = 0u8;
        for p in (1..sigma.len()).step_by(2) {
            // 偶数次項の微分だけ残る（GF(2)微分）
            den ^= gf.mul(sigma[p], gf.pow((gf.log(xinv) as usize) * (p - 1)));
        }
        if den == 0 {
            return None;
        }
        // fcr=0: e = X^(1-fcr) * omega/sigma' = X * omega/sigma' … fcr=0 → X^1
        let e = gf.mul(gf.mul(num, gf.inv(den)), x);
        out[idx] ^= e;
    }

    if is_codeword(&out) {
        Some(out)
    } else {
        None
    }
}

impl Gf {
    #[inline]
    fn log(&self, a: u8) -> u8 {
        self.log[a as usize]
    }
}

fn poly_sub(a: &[u8], b: &[u8]) -> Vec<u8> {
    let n = a.len().max(b.len());
    let mut out = vec![0u8; n];
    for i in 0..n {
        let av = if i < a.len() { a[i] } else { 0 };
        let bv = if i < b.len() { b[i] } else { 0 };
        out[i] = av ^ bv; // GF(2): sub = add = xor
    }
    out
}

fn poly_mul_mod(gf: &Gf, a: &[u8], b: &[u8], moddeg: usize) -> Vec<u8> {
    let mut out = vec![0u8; moddeg];
    for (i, &ai) in a.iter().enumerate() {
        if ai == 0 {
            continue;
        }
        for (j, &bj) in b.iter().enumerate() {
            if i + j < moddeg {
                out[i + j] ^= gf.mul(ai, bj);
            }
        }
    }
    out
}

/// 生成多項式 g(x)=Π_{j=0}^{15}(x + alpha^j)（fcr=0, GF(2)）を高次先頭で返す（長さ17, G[0]=1）。
fn genpoly_high_first(gf: &Gf) -> Vec<u8> {
    // 低次先頭で構築
    let mut g = vec![1u8]; // 定数1
    for j in 0..NROOTS {
        let r = gf.pow(j);
        let mut ng = vec![0u8; g.len() + 1];
        for k in 0..g.len() {
            ng[k] ^= gf.mul(r, g[k]); // 定数項 r*g
            ng[k + 1] ^= g[k]; // x*g
        }
        g = ng;
    }
    g.reverse(); // 高次先頭に（G[0]=x^16 の係数=1）
    g
}

/// RS符号化（テスト用）：188バイト → 204バイト（16パリティを末尾に付加）。
/// `msg[0]` が最高次。parity = msg(x)·x^16 mod g(x)。
pub fn encode(msg: &[u8]) -> Vec<u8> {
    assert_eq!(msg.len(), K);
    let gf = build_gf();
    let g = genpoly_high_first(&gf); // 長さ17, 高次先頭, G[0]=1

    // work = [msg(188) , 0*16]（高次先頭, 長さ204）で多項式長除算
    let mut work = msg.to_vec();
    work.extend(std::iter::repeat(0u8).take(NROOTS));
    for i in 0..K {
        let coef = work[i];
        if coef != 0 {
            for k in 0..g.len() {
                work[i + k] ^= gf.mul(coef, g[k]);
            }
        }
    }
    // 余り = work[K..N]（16バイト, 高次先頭）
    let mut out = msg.to_vec();
    out.extend_from_slice(&work[K..N]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(s: &mut u32) -> u8 {
        *s = s.wrapping_mul(1664525).wrapping_add(1013904223);
        (*s >> 16) as u8
    }

    #[test]
    fn encode_is_codeword() {
        let mut s = 1u32;
        let msg: Vec<u8> = (0..K).map(|_| lcg(&mut s)).collect();
        let cw = encode(&msg);
        assert_eq!(cw.len(), N);
        assert!(is_codeword(&cw), "符号化結果が有効符号語でない");
    }

    #[test]
    fn corrects_up_to_8_errors() {
        let mut s = 42u32;
        let msg: Vec<u8> = (0..K).map(|_| lcg(&mut s)).collect();
        let cw = encode(&msg);
        let mut rx = cw.clone();
        // 相異なる8位置に非0誤りを入れる（訂正能力 t=8）
        let positions = [3usize, 20, 55, 88, 120, 150, 180, 200];
        for (i, &pos) in positions.iter().enumerate() {
            rx[pos] ^= (i as u8) + 1; // 1..8 の非0誤り
        }
        let dec = decode(&rx).expect("訂正できない");
        assert_eq!(&dec[..K], &msg[..], "訂正後メッセージ不一致");
    }

    #[test]
    fn corrects_single_error() {
        let mut s = 7u32;
        let msg: Vec<u8> = (0..K).map(|_| lcg(&mut s)).collect();
        let cw = encode(&msg);
        let mut rx = cw.clone();
        rx[42] ^= 0x9d;
        let dec = decode(&rx).expect("1誤り訂正できない");
        assert_eq!(&dec[..], &cw[..], "1誤り訂正後が符号語と不一致");
    }
}
