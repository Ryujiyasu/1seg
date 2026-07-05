---
status: published
url: https://yasu-home.com/isdbt-1seg-tmcc-and-cn-wall/
wp_post_id: 199
series: 自作ワンセグ復調器をつくる
part: 3
title: 電波に局の設定を喋らせる、そして壁にぶつかる ── 自作ワンセグ復調器#3：TMCC復号と、C/Nの壁を較正で証明する
slug: isdbt-1seg-tmcc-and-cn-wall
category: IoT・組込み  # term_id 40
tags: [RTL-SDR, SDR, ISDB-T, ワンセグ, TMCC, Viterbi, FEC, デインターリーブ, Rust, DSP]
featured_image: 03-tmcc-frame-scan.png      # アイキャッチ＝TMCCフレーム位相スキャン（attachment 197）
images:
  - blog/images/03-tmcc-frame-scan.png       # TMCCフレーム位相スキャン（phase111が99.5%で突出, アイキャッチ）attachment 197
  - blog/images/03-fec-cn-wall.png           # FEC 12設定 vs 雑音床（誰も超えない）attachment 198
note: |
  公開HTMLは 03-tmcc-and-cn-wall.html（Gutenberg）。2幕構成。
  幕1=TMCC復号（DBPSK・204シンボルフレーム・phase111スパイク・Layer A QPSK 2/3 1seg / B 64QAM 12seg）。
  幕2=④デインタ＋⑤Viterbiを組むも未ロック→再エンコード一致率を「雑音床(0.928)」で較正し、実信号0.894<床=C/Nの壁を証明。
  ソース: crates/isdbt-dsp（tmcc.rs, deinterleave.rs, demap.rs, viterbi.rs）, examples/{tmcc_scan,fec_lock_probe}.rs。
  ※未公開ドラフト。公開時：wp media import で03-*.pngを上げ、HTMLの wp-image-XXX / -1024xNNN を実値に差し替え、category term_id=40、featured=03-tmcc-frame-scan。
  次回#4の含み: 壁アンテナ(F→SMA)でC/Nを上げてFECロック→TS。
---

④TMCC復号で関西テレビch17の伝送パラメータ（Layer A=QPSK/2-3/1seg, B=64QAM/12seg, 部分受信=1）を実電波から読み、
さらに④デインタ＋⑤Viterbiまで実装して全チェーンを完走させた記録。
山場は2つ：(幕1) TMCCフレーム位相を204総当たりして phase111 が同期語99.5%でスパイク＝局の設定が読めた、
#2で見たQPSKをTMCCが独立に裏付け。(幕2) FECがロックせず → 再エンコード一致率を純雑音(床0.928)で較正し、
実信号0.894＜床＝畳み込み構造が出ていない＝C/Nの壁、と証明。打開は壁アンテナ。
