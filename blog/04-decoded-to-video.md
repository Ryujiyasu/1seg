---
status: published
url: https://yasu-home.com/isdbt-1seg-decoded-to-video/
wp_post_id: 201
series: 自作ワンセグ復調器をつくる
part: 4
title: 実電波が、映像になった ── 自作ワンセグ復調器#4：壁アンテナでC/Nの壁を越え、逆拡散→RSでMPEG-TSを解き、テレビが映るまで
slug: isdbt-1seg-decoded-to-video
category: IoT・組込み
tags: [RTL-SDR, SDR, ISDB-T, ワンセグ, MPEG-TS, リードソロモン, エネルギー拡散, H.264, Rust, DSP]
featured_image: 04-decoded-frame.png        # アイキャッチ＝復号したテレビ映像1フレーム
images:
  - blog/images/04-decoded-frame.png         # 実電波から復号したTV映像（関西テレビ, 320x180, アイキャッチ）
note: |
  公開HTMLは 04-decoded-to-video.html（Gutenberg）。シリーズ最終回・完成編。
  幕1=壁アンテナでC/N突破（コヒーレンス0.84→1.000, EVM98%→16.3%, FEC0.894→1.00000）。
  幕2=0x47は出るがRS不成立（Forney branch-0でsyncは遅延0=生き残る罠）。
  幕3=二つの取り違えを発見：チェーン順序（ISDB-TはRS→スクランブルなのでRXは逆拡散→RS）、
      PRBSリセット周期（8でなく≈64）。成功PID(0x1fff/0x0151…)で本物TS確定→RS99.7%。
  幕4=ffprobeで H.264 320x180 + AAC を確認、ffmpegで実際のTV映像1フレーム抽出。
  ソース: crates/isdbt-dsp（全モジュール）, examples/ts_final.rs。
  再現: cargo run --release -p isdbt-dsp --example ts_final -- cap.iq 1015873 out.ts 11000
        ffmpeg -ss 5 -i out.ts -frames:v 1 frame.png
  ※未公開ドラフト。公開時：wp media import で 04-decoded-frame.png を上げ（320x180なので原寸URL、-1024xNNN無し）、
    HTML内 wp-image-XXX と /2026/07/ のパスを実attachment/実URLに差し替え、category term_id=40、featured=04-decoded-frame。
  ※映像は関西テレビの実放送フレーム。ブログ掲載は引用の範囲・小サイズ(320x180)・技術解説目的。気になれば構成をぼかす/差し替え可。
---

シリーズ完成編。#3で当たったC/Nの壁を壁アンテナ(F→SMA)で突破し、FECが誤り0でロック(一致率1.00000)。
そこから ⑥ を詰めた記録：0x47は出るがRS不成立→(1)チェーン順序がDVBと逆でISDB-TはRS→スクランブル＝RXは逆拡散が先、
(2)PRBSリセット周期が8でなく≈64、の二点を、成功パターン可視化と成功PID表示で突き止めてRS99.7%。
ffprobeで H.264 320×180 + AAC を確認し、ffmpegで実際のテレビ映像(関西テレビ, 9:39の朝番組)を1フレーム抽出。
IQ→自作Rustコードのみ→動く映像、まで到達。①〜⑥完成。
