# ブラウザ版 — WASM + WebUSB で「URLを開くと映る」

自作ワンセグ復調器（Rust製 `isdbt-dsp`）を **WebAssembly** にして、ブラウザ内で
IQ → MPEG-TS に復号し、`mpegts.js`（MSE）で H.264+AAC を再生する。

- **中核（検証済み）**：`isdbt-wasm` → `pkg/`。IQを `feed(u8)` すると TS が返る。
  Node での実データ検証で **ネイティブと同一の 3376 パケット・H.264 320×180 + AAC** を確認済み。
- **ファイルモード（確実）**：`.iq`（rtl_sdr 生u8, fs=1015873）を選ぶ → ブラウザ内で復号 → 再生。
- **ライブモード（実機検証済み）**：WebUSB で RTL2832U を直結して生放送を復号（`rtlsdr.js`）。
  実機（RTL-SDR Blog V4 = R828D）に対し、`rtlsdr.js` を無改変で navigator.usb 経由で走らせ、
  **地上波ワンセグを捕獲 → 復号率 100%（オートゲイン）→ 鮮明なTV画**を確認済み。
  ドライバはチューナを自動検出（R820T=0x34 / R828D=0x74）、3.57MHz 低IF＋実ADC、フィルタ校正付き。

## ビルドと起動

```bash
# WASM を再ビルド（ソース変更時）
wasm-pack build crates/isdbt-wasm --target web --out-dir ../../web/pkg --release

# web/ を配信（WebUSB は localhost か HTTPS が必要）
cd web && python3 -m http.server 8000
# → Chrome/Edge で http://localhost:8000 を開く
```

## 使い方

1. **ファイルから**：`captures/*.iq` を選ぶ → 数秒で復号 → 動画が再生。
   うまく再生されない環境でも「TSをダウンロード」で `.ts` を保存し `ffplay`/VLC で確認可。
2. **ライブ（WebUSB）**：「RTL-SDR に接続」→ デバイスを許可 → 生放送を連続復号。
   ※ `dvb_usb_rtl28xxu` 等のカーネルドライバがデバイスを掴んでいると WebUSB から開けない。
   Linux では blacklist 済み前提（`rts-sdr-v4-driver-setup` 参照）。ゲインは auto 推奨。
   `CENTER`（周波数）は `main.js` 冒頭で放送局に合わせて変更する。

## ライブ経路の検証方法（実機・ヘッドレス）

ブラウザの WebUSB は headless では叩けないが、node-usb の WebUSB 実装（`navigator.usb` 互換）を
差し込めば `rtlsdr.js` を無改変で実機テストできる：

```js
import { WebUSB } from "usb";                    // npm i usb
const webusb = new WebUSB({ devicesFound: ds => ds.find(d => d.vendorId === 0x0bda) });
Object.defineProperty(globalThis, "navigator", { value: { usb: webusb }, configurable: true });
const { RtlSdr } = await import("./rtlsdr.js");
const sdr = await RtlSdr.request();
await sdr.open({ frequency: 497142857, sampleRate: 1015873, gain: null }); // auto gain
await sdr.readSamples(u8 => { /* → decoder.feed(u8) */ });
```

取得IQを `isdbt-dsp` の `decode`/`stream_decode` に通すと 100% 復号する。

## 構成

- `pkg/` … `wasm-pack` の出力（`isdbt_wasm_bg.wasm` ≈ 246KB, 生成物）
- `main.js` … WASMロード → ファイル/ライブのIQを `feed` → TSを `mpegts.js` へ
- `rtlsdr.js` … WebUSB RTL2832 ドライバ（初期化・PLL・バルク読み。実機調整前提）
- `index.html` … UI（`mpegts.js` は CDN 読み込み）
