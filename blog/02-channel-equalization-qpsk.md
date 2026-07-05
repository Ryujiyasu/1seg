---
status: published
url: https://yasu-home.com/isdbt-1seg-channel-equalization-qpsk/
wp_post_id: 196
series: 自作ワンセグ復調器をつくる
part: 2
title: ワンセグのQPSKが見えた ── 自作ワンセグ復調器#2：スキャッタードパイロットでチャネルを割り、電波自身に答え合わせをさせる
slug: isdbt-1seg-channel-equalization-qpsk
category: IoT・組込み  # term_id 40
tags: [RTL-SDR, SDR, ISDB-T, ワンセグ, OFDM, チャネル等化, パイロット, QPSK, Rust, DSP]
featured_image: 02-equalized-qpsk.png       # アイキャッチ＝等化後コンスタレーション（attachment 195）
images:
  - blog/images/02-bin-coherence-scan.png   # bin整列スキャン（電波が296を指す）attachment 194
  - blog/images/02-equalized-qpsk.png        # 等化後QPSK（全 vs 良C/N、アイキャッチ）attachment 195
note: |
  公開HTMLは 02-channel-equalization-qpsk.html（Gutenberg）。
  本文要素＝SP/PRBS生成則、罠①セグメント絶対オフセット2592、検算（隣接SPコヒーレンス：bin296圧勝・symbol%4が+1整合）、QPSK確認、ボケ=C/N。
  ソース/コード: crates/isdbt-dsp（pilots.rs, equalize.rs）, examples/equalize_probe.rs。
  ※未公開ドラフト。公開時：wp media import で02-*.pngを上げ、HTML内の wp-image-XXX / -1024xNNN を実attachment/実寸URLに差し替え、category term_id=40、featured=02-equalized-qpsk。
---

③ チャネル等化を実装し、関西テレビ ch17 の生IQから**ワンセグのQPSKコンスタレーション**を出すまでの記録。
山場は2つ：(罠①) 1セグ＝中央セグメントが全帯域の絶対キャリア**2592**から始まるという急所、
(検算) 隣接SPコヒーレンスで「同期・切り出し位置・PRBSオフセット・symbol%4」を一発裏取り
（bin296がコヒーレンス0.82で圧勝、symbol%4が+1/シンボルで整合）。
出たQPSKのボケはバグでなくC/N（弱アンテナ）で、良シンボルだけ選ると理想点に締まる。
