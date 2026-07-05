//! ⑥段（バイトインターリーブ↔デインターリーブ＋RS＋整列）の自己整合を合成データで検証。
//! これが通れば ⑥段は正しく、実データのRS不成立はビット段（上流）が原因、と局在化できる。

use isdbt_dsp::rs;
use isdbt_dsp::ts::{best_sync_phase, ByteDeinterleaver, ByteInterleaver, BI_I, BI_M, SYNC, TSP};

fn lcg(s: &mut u32) -> u8 {
    *s = s.wrapping_mul(1664525).wrapping_add(1013904223);
    (*s >> 16) as u8
}

fn main() {
    let mut seed = 2024u32;
    // 1) 既知のRS符号語ブロックを多数（byte0=0x47 sync）
    let nblocks = 400usize;
    let mut rs_stream: Vec<u8> = Vec::new();
    for _ in 0..nblocks {
        let mut msg = vec![0u8; 188];
        msg[0] = SYNC;
        for b in msg.iter_mut().skip(1) {
            *b = lcg(&mut seed);
        }
        let cw = rs::encode(&msg); // 204バイト, byte0=0x47
        assert!(rs::is_codeword(&cw));
        rs_stream.extend_from_slice(&cw);
    }

    // 2) 送信側 Forneyバイトインターリーブ
    let mut il = ByteInterleaver::new();
    let interleaved: Vec<u8> = rs_stream.iter().map(|&b| il.push(b)).collect();

    // 3) 受信側でオフセットを付けて（実データを模して）デインターリーブ
    let channel_offset = 55usize; // 実データの sync phase を模擬
    let feed: Vec<u8> = interleaved[channel_offset..].to_vec();

    let latency = BI_M * BI_I * (BI_I - 1);
    println!("=== 合成⑥往復（コミュテータ総当たり）===");
    let checker = rs::Checker::new();
    let mut best = (0usize, 0usize);
    for c in 0..BI_I {
        let mut di = ByteDeinterleaver::new();
        let stream: Vec<u8> = feed[c..]
            .iter()
            .enumerate()
            .filter_map(|(j, &b)| {
                let o = di.push(b);
                (j >= latency).then_some(o)
            })
            .collect();
        let (phase, score) = best_sync_phase(&stream);
        // RS有効ブロック数
        let mut ok = 0usize;
        let mut tot = 0usize;
        let mut i = phase;
        while i + TSP <= stream.len() && tot < 100 {
            if checker.is_codeword(&stream[i..i + TSP]) {
                ok += 1;
            }
            tot += 1;
            i += TSP;
        }
        println!(
            "  c={c:2}: sync {:.0}% phase={phase} RS有効 {ok}/{tot}",
            score * 100.0
        );
        if ok > best.0 {
            best = (ok, c);
        }
    }
    println!(
        "\n最良 c={} でRS有効 {} ブロック → {}",
        best.1,
        best.0,
        if best.0 > 50 {
            "✅ ⑥段は自己整合（バグはビット段=上流）"
        } else {
            "✗ ⑥段のモデルが誤り（インタ/デインタ/整列を要修正）"
        }
    );
}
