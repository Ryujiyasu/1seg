---
status: published
url: https://yasu-home.com/isdbt-1seg-ofdm-sync-lock/
wp_post_id: 193
series: 自作ワンセグ復調器をつくる
part: 1
title: GI=1/32と出た、でも嘘だった ── 自作ワンセグ復調器#1：CP自己相関で実電波にロックし、周期性でGIを当て直す
slug: isdbt-1seg-ofdm-sync-lock
category: IoT・組込み
tags: [RTL-SDR, SDR, ISDB-T, ワンセグ, OFDM, Rust, DSP]
featured_image: 01-cp-correlation-lock.png  # attachment 191
images:
  - blog/images/01-osaka-uhf-scan.png      # attachment 192（実電波スキャン）
  - blog/images/01-cp-correlation-lock.png # attachment 191（②ロック / アイキャッチ）
note: 公開HTMLは 01-ofdm-sync-lock.html（Gutenberg）。本文要素＝CP相関の式、sync_probe出力、GI周期性スコア表。
---

実電波（関西テレビ ch17・生駒山）で ② OFDM同期がロックするまでの記録。
GI判定を「単発メトリクス最大」でやって 1/32 に誤判定 → 周期性（折り畳み）で 1/8 を当て直した話。
ソース/コード: crates/isdbt-dsp（sync.rs）, examples/sync_probe.rs。
