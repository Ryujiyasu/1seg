---
status: published
url: https://yasu-home.com/isdbt-1seg-in-browser-wasm-webusb/
wp_post_id: 203
series: 自作ワンセグ復調器をつくる
part: 5
title: 「URLを開くとテレビが映る」を、本当にやる ── 自作ワンセグ復調器#5：Rust復調器をWASMにして、WebUSBでドングルを直結し、ブラウザの中で地デジが映像も音声も鳴るまで
slug: isdbt-1seg-in-browser-wasm-webusb
category: IoT・組込み
tags: [RTL-SDR, SDR, ISDB-T, ワンセグ, WebAssembly, WASM, WebUSB, WebCodecs, AudioWorklet, Rust, ブラウザ]
featured_image: 05-browser-live.png       # アイキャッチ＝ブラウザ版アプリでライブTVが映っている画面
images:
  - blog/images/05-browser-live.png        # 実アプリのスクショ。canvasに実放送（関西テレビ）、上にWebUSBボタン
note: |
  シリーズ完結編。#4の「次はWASM+WebUSBでブラウザに載せたい……それはまた別の話」を回収する回。
  実物: https://yasu-home.com/1seg/ （開いてドングルを挿すと映る）。ソース: https://github.com/Ryujiyasu/1seg
  構成: crates/isdbt-wasm（WASM）, web/{index.html,main.js,rtlsdr.js,decoder-worker.js,pcm-worklet.js,pkg/}
  技術の肝:
    - WASM: 復調器(crates/isdbt-dsp)がそのままwasm32へ（rustfft/num-complex全部通る）。StreamingDecoder.feed(IQ)->TS。
    - WebUSB: RTL2832Uドライバを自作JSで。V4のチューナは【R828D＝I2Cアドレス0x74】(R820Tの0x34ではない)、
      さらに【ゼロIFでなく3.57MHz低IF＋実ADC】。librtlsdr/rtlsdrjs準拠に直して実機で復号率100%。
    - 実時間の罠: 検証はずっと「IQをファイルに保存→後でオフライン復号」で、実時間給餌を通していなかった。
      ブラウザの実時間で一気に噴出: (a)未ロック時バッファ暴走でWASM OOM, (b)弱搬送波でH≈0→等化Inf/NaN→
      Viterbiパニック, (c)重い同期計算がUSB読取をブロック→FIFO溢れ→多重バッファ読みで解決,
      (d)映ったが動かない=メインスレッド飽和→Web Worker+OffscreenCanvasで復号/デコード/描画を隔離。
    - 音声: AAC(ADTS)→WebCodecs AudioDecoder→AudioWorkletのリングバッファで連続再生。
      A/Vズレ=起動バックログを映像は早送り/音声は等速で追えず→ロック直後にライブエッジへ飛ぶ
      （整列維持のため204シンボル=1 OFDMフレーム=PRBS1周期単位で捨てる）。立ち上がりのAAC整定は先頭数フレーム破棄。
  ※未公開ドラフト。公開時: wp media import で 05-browser-live.png を上げ、HTML内の wp-image-XXX / uploads/2026/07/ を
    実attachment/実URLに差し替え、category term_id=40、featured=05-browser-live。
  ※アイキャッチのcanvasは関西テレビの実放送。技術解説目的・小サイズ引用。気になれば差し替え可。
---

シリーズ完結編。#4の最後に書いた「次はWASM＋WebUSBでブラウザに載せて『URLを開くとテレビが映る』ところまで持っていきたい……が、それはまた別の話」を、本当にやった回。
Rust製の復調器①〜⑥をまるごとWebAssemblyにし、WebUSBでRTL-SDRをブラウザから直結。届いた壁は全部越えた：
V4のチューナが【R828D＝I2C 0x74】で3.57MHz低IFだった発見、オフライン検証では出なかった実時間の罠
（OOM・NaN・サンプル欠落・メインスレッド飽和）、Web Worker＋OffscreenCanvasで映像を、WebCodecs＋AudioWorkletで音声を。
最後にA/Vのズレをロック直後の「ライブエッジ飛び」で解消。結果、**ブラウザのタブの中だけで、生の地デジワンセグが映像も音声も再生できる**。
実物: https://yasu-home.com/1seg/ 。ソース: https://github.com/Ryujiyasu/1seg 。GNU Radio非依存、フルスクラッチ、①〜⑥完結。
