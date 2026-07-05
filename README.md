# RTS_SDR — 自作ワンセグ（ISDB-T 1seg）復調器

RTL-SDR Blog V4 で受けた IQ から、**ISDB-T のワンセグ（1セグ）を自前で復調する**プロジェクト。
Rust実装 → 実電波で**H.264映像まで復号**（バッチ `decode` / 連続ライブ `stream_decode`）→
**WebAssembly でブラウザ内復調**（`web/`）まで到達。「URLを開くと（IQを渡すと）テレビが映る」。

## なぜ自作か

- フルセグ（12セグ・帯域 約5.6MHz）は RTL-SDR の約2.5MHz窓に入りきらない → 1本では不可。
- ワンセグ（中央1セグ・約429kHz）なら窓に収まり、無スクランブルなのでそのまま再生できる。
- 既存の `gr-isdbt` は GNU Radio 3.7〜3.8＋SWIG 世代の化石ビルドで現代環境では動かしづらい → 自分で書く。

## 段構成（信号の流れ）

| 段 | 内容 | 状態 |
|----|------|------|
| ① | RF入力（rtl_sdr の生IQ → 複素サンプル） | ✅ `iq.rs` |
| ② | **OFDM同期**（CP自己相関でシンボル境界＋小数CFO） | ✅ `sync.rs`（実装＋合成IQ＋実機で検証） |
| ③ | **チャネル等化**（スキャッタードパイロット） | ✅ `pilots.rs`/`equalize.rs`（実機ch17でQPSK確認） |
| ④ | デマップ＋デインターリーブ（要TMCC） | ✅ `tmcc.rs`/`deinterleave.rs`/`demap.rs`（TMCC・周波数/時間デインタ・QPSKデマップ・bitデインタ） |
| ⑤ | FEC（Viterbi＋RS(204,188)） | ✅ `viterbi.rs`（実機でFECロック・一致率1.000）／`rs.rs`（RS復号・実機99.7%成功） |
| ⑥ | TS出力 | ✅ `ts.rs`（**実電波→MPEG-TS、RS成功99.7%、H.264 320×180＋AAC を ffmpeg で再生確認**） |

> **🎉 完成（2026-07-01）**：本線の `decode` 例が実IQ（壁アンテナ）を **MPEG-TS** に単一パスで復号
> （RS成功99.7%）。ffprobe で **H.264 映像 320×180 ＋ AAC 音声 48kHz** を確認、フレーム抽出で
> **実際のテレビ映像**（関西テレビ）を取り出せた。IQ → 同期 → 等化 → TMCC → デインタ → Viterbi
> → 逆拡散 → RS → TS の全段が実電波で貫通。
> ```bash
> cargo run --release -p isdbt-dsp --example decode -- cap.iq 1015873 out.ts 11000
> ffmpeg -ss 5 -i out.ts -frames:v 1 frame.png   # 映像を1枚抜く
> ```
> 補足：エネルギー拡散のリセットは **1 OFDMフレーム = 64 RSブロックごと**
> （204sym × 384carrier × 2bit × 2/3 ÷ 8 ÷ 204 = 64）。
>
> **リアルタイム（ストリーミング）**：`stream_decode` は IQ(stdin/file)→TS(stdout/file) を
> 単一パスで復号（`ViterbiStreaming` 使用）。**実電波のライブパイプで動作確認済み（99%復号）**：
> ```bash
> # ライブ受信→復号（head でクリーンにEOFを渡すのがコツ。timeout だと閉じ方が不安定）
> rtl_sdr -f 497142857 -s 1015873 -g 30 - | head -c 40000000 | \
>   cargo run --release -q -p isdbt-dsp --example stream_decode -- - live.ts
> ffmpeg -ss 6 -i live.ts -frames:v 1 frame.png     # ライブ映像を1枚
> ```
> 表示端末なら末尾を `| ffplay -` にすれば**連続ライブ再生**（stdin逐次読み・都度flush・
> バッファはcursor＋compactで有界なので無限ストリームOK）。実電波のライブニュースを
> 連続復号して映像化できることを確認済み。
> `decode`＝バッチ本線、`stream_decode`＝リアルタイム/連続ライブ。他の `examples/` は各段の診断用。
>
> **ブラウザ版（WASM+WebUSB）**：`crates/isdbt-wasm` を WebAssembly 化し、ブラウザ内で
> IQ→MPEG-TS を復号→`mpegts.js` で再生。Nodeで**ネイティブと同一のH.264 320×180+AAC**を検証済み。
> 詳細と起動手順は [web/README.md](web/README.md)（`wasm-pack build … --target web` → `python3 -m http.server`）。

## ビルド & テスト

```bash
cargo test -p isdbt-dsp
```

## ハードを動かす（母艦：Ubuntu 26.04 で確認済み）

```bash
sudo apt-get install -y rtl-sdr libusb-1.0-0-dev
echo -e "blacklist dvb_usb_rtl28xxu\nblacklist rtl2832_sdr" \
  | sudo tee /etc/modprobe.d/blacklist-rtlsdr.conf
sudo modprobe -r rtl2832_sdr dvb_usb_rtl28xxu
rtl_test        # → "RTL-SDR Blog V4 Detected" / "R828D" / loss 0 を確認
```

## 参照

- 一次資料：**ARIB STD-B31**（地上デジタル伝送方式）
- 参照実装：`ref/gr-isdbt`（ISDB-T固有処理）、`ref/DAB-Radio`（GNU Radio非依存の単体実装の手本）
- 開発記（ブログ）：<https://yasu-home.com/rtl-sdr-v4-ubuntu-2604-setup/>（#0 環境構築）

> `ref/` は別リポジトリの浅cloneで、`.gitignore` 済み。再取得：
> ```bash
> git clone --depth 1 https://github.com/git-artes/gr-isdbt.git ref/gr-isdbt
> git clone --depth 1 https://github.com/williamyang98/DAB-Radio.git ref/DAB-Radio
> ```
