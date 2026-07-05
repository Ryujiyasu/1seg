---
status: published
url: https://yasu-home.com/rtl-sdr-v4-ubuntu-2604-setup/
wp_post_id: 190
series: 自作ワンセグ復調器をつくる
part: 0
title: 「V4は専用ドライバが要る」は半分ウソだった ── RTL-SDR Blog V4 を Ubuntu 26.04 の apt だけで動かす（自作ワンセグ復調器 #0）
slug: rtl-sdr-v4-ubuntu-2604-setup
category: IoT・組込み
tags: [RTL-SDR, SDR, ISDB-T, ワンセグ, Ubuntu, Linux]
note: 公開HTML(Gutenberg)は 00-rtl-sdr-v4-setup.html。本文先頭にV4本体写真(attachment 189)。
---

# RTL-SDR Blog V4、Ubuntu 26.04であっさり動いた（自作ワンセグ復調器をつくる #0）

## いきさつ

「SDRで地デジTVって見れるの？」という素朴な疑問から始まった。結論から言うと、**フルセグ（12セグ・帯域約5.6MHz）はRTL-SDRの一度に覗ける窓が約2.5MHzしかない**ので、1本では物理的に入りきらない。アンテナをどれだけ良くしても窓の狭さは解決しない。

ただし**ワンセグ（中央の1セグメントだけ・帯域約429kHz）なら2.5MHzの窓に余裕で収まる**。しかもワンセグは無スクランブルなので、復調できればそのまま再生できる。

既存の `gr-isdbt`（ウルグアイの大学発）にワンセグ復調の実績はあるが、GNU Radio 3.7〜3.8＋SWIG世代の“化石ビルド”で、いまの環境で素直に通る代物ではない。

> だったら自分でワンセグ復調器を書く。

——というのがこのシリーズ。記念すべき #0 は環境構築から。買ったのは **RTL-SDR Blog V4（R828Dチューナー + RTL2832U）**。

## ハマりどころ1：「V4は専用ドライバが要る」は新しめのディストリなら気にしなくていい

ネットだと「V4はR828Dなので `rtl-sdr-blog` の専用ドライバを入れろ、旧 osmocom ドライバだと動かない」とよく書かれている。これは半分本当で、半分はもう古い情報。

試しに **Ubuntu 26.04 の apt 版（`rtl-sdr 2.0.2`）をそのまま入れたら、V4を正式認識して完動した**。ソースビルド不要だった。

```bash
sudo apt-get install -y rtl-sdr libusb-1.0-0-dev
```

## ハマりどころ2：カーネルの地デジ（DVB-T）ドライバがデバイスを横取りする

挿すとLinuxカーネルが `dvb_usb_rtl28xxu`（＋ `rtl2832_sdr` など）でデバイスを掴んでしまい、SDRとして開けない。`/etc/modprobe.d/` でblacklistして外す。

```bash
echo -e "blacklist dvb_usb_rtl28xxu\nblacklist rtl2832_sdr" \
  | sudo tee /etc/modprobe.d/blacklist-rtlsdr.conf
sudo modprobe -r rtl2832_sdr dvb_usb_rtl28xxu
```

再起動でも外れるが、`modprobe -r` でその場でアンロードできた。

## 動作確認

```bash
rtl_test
```

```
Found 1 device(s):
  0:  RTLSDRBlog, Blog V4, SN: 00000001

Using device 0: Generic RTL2832U OEM
Found Rafael Micro R828D tuner
RTL-SDR Blog V4 Detected
Supported gain values (29): 0.0 0.9 1.4 ... 48.0 49.6
Sampling at 2048000 S/s.
...
Samples per million lost (minimum): 0
```

`R828D` を認識、`RTL-SDR Blog V4 Detected` も出て、**サンプルロス0**。文句なし。

## 次回

ワンセグの周波数でIQ（複素サンプル）を録って、**OFDM同期**から手を付ける。ガードインターバル（CP）の自己相関でシンボル境界と周波数オフセットを拾う、OFDMの定番処理。ここが復調器の土台になる。

<!-- TODO(本番前): rtl_testの全文ログ・写真を差し込む / gr-isdbt・DAB-Radioへのリンクを貼る -->
