//! ISDB-T 1セグ復調器の **WASM バインディング**。
//! ブラウザから IQ の u8 バイト列を `feed` すると、MPEG-TS バイトが返る。
//! WebUSB で RTL2832 から取ったIQを流し込み、返ったTSを mpegts.js 等で再生する想定。
//!
//! ビルド：`wasm-pack build crates/isdbt-wasm --target web`（ブラウザ）/ `--target nodejs`（検証）。

use isdbt_dsp::StreamingDecoder;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn error(msg: &str);
}

/// パニック時に console.error へ実メッセージ（場所・理由）を出す。既定の不透明な
/// `RuntimeError: unreachable` を、診断可能なメッセージに変える。
fn set_panic_hook() {
    use std::sync::Once;
    static SET: Once = Once::new();
    SET.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            error(&format!("WASM panic: {info}"));
        }));
    });
}

/// ストリーミング復調器（1インスタンスで1チャンネルを連続復号）。
#[wasm_bindgen]
pub struct WasmDecoder {
    inner: StreamingDecoder,
}

#[wasm_bindgen]
impl WasmDecoder {
    /// 新しいデコーダを作る。以降 `feed` にIQを流し続ける。
    #[wasm_bindgen(constructor)]
    pub fn new() -> WasmDecoder {
        set_panic_hook();
        WasmDecoder {
            inner: StreamingDecoder::new(),
        }
    }

    /// IQ（rtl_sdr 生u8, I,Q インターリーブ）を投入し、生成された MPEG-TS バイトを返す。
    /// まだ同期/整列ロック前や生成が無い場合は空配列。届いたぶんを全処理する。
    pub fn feed(&mut self, iq: &[u8]) -> Vec<u8> {
        self.inner.feed(iq)
    }

    /// ライブモード：ロック直後にバックログを捨ててライブエッジから（A/V同期・低遅延）。
    #[wasm_bindgen(js_name = setLive)]
    pub fn set_live(&mut self, live: bool) {
        self.inner.set_live(live);
    }

    /// IQ を投入するだけ（処理は `pump` で小分けに）。ライブ描画を詰まらせないための分割用。
    pub fn push(&mut self, iq: &[u8]) {
        self.inner.push(iq);
    }

    /// バックログを少しだけ処理して生成TSを返す（`push`→`pump`ループで使う）。
    pub fn pump(&mut self) -> Vec<u8> {
        self.inner.pump()
    }

    /// 未処理シンボル数（`pump` をどれだけ回すべきかの目安）。
    pub fn backlog(&self) -> usize {
        self.inner.backlog_syms()
    }

    /// 同期＋整列がロックできたか。
    #[wasm_bindgen(js_name = isLocked)]
    pub fn is_locked(&self) -> bool {
        self.inner.is_locked()
    }

    /// これまでにRS復号できたTSパケット数。
    pub fn decoded(&self) -> usize {
        self.inner.stats().0
    }
}

impl Default for WasmDecoder {
    fn default() -> Self {
        Self::new()
    }
}
