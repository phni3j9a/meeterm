# Mock assets

## meerkat-watch.webp（第2版の素材、現在は未使用）

2026-09-07に、このモックのために組み込みImageGenで生成したオリジナルの挿絵。画像モデル名を指定する引数は提供されていないため、特定のモデル名による生成を確認したものではない。

空のワークスペースなど、作業を始める前の状態で使う。ターミナル本文には置かない。透明PNGの生成結果を確認し、透明度を保った480×480のWebPへ縮小した。

生成に使用したプロンプト:

> Use case: illustration-story. Asset type: refined editorial spot illustration for the empty workspace screen of meeterm, a mature minimal professional SSH terminal mobile app. Create one original full-body meerkat standing upright on a small smooth stone, calmly alert, looking slightly to the upper right. Character: recognizably natural meerkat, elegant long body, small ears, dark eye markings, narrow snout, tucked paws, long tail curving to one side. Style: beautiful mid-century natural-history book illustration, restrained hand-ink contours and delicate engraved hatching with a few solid ink shapes, subtle imperfect print texture. Contemporary Swiss editorial refinement, not childish or cute, not cartoon mascot, not 3D. Two ink colors only: very deep charcoal forest #283B30 and muted sage #A9C4AE. Real transparent alpha background, no paper rectangle, no scenic landscape, no text, no lettering, no logos, no terminal or computer. Large clean negative space around the isolated animal, balanced square composition, entire figure fits without crop and occupies center 60 percent. Detailed enough to look crafted at 240px, simple enough to read at 110px. This is a quiet companion watching over a durable remote workspace. Deliver highest-quality crisp bitmap with transparency.

最適化済み画像はこのディレクトリに保存。モック本体で使用する際はdata URLとして埋め込み、HTML単体での利用を保つ。

## mplus1code-regular.woff2

製品に既に同梱されている `modules/meeterm-terminal/android/src/main/assets/fonts/MPLUS1Code[wght].ttf` から、ウェイト450を固定してWOFF2へ変換した。日本語を含む元の文字集合を保持し、約669KiBに圧縮。ネイティブ側のフォントや依存関係は変更していない。

著作権: The M+ FONTS Project Authors (2021)。SIL Open Font License 1.1。全文は [OFL-MPLUS1Code.txt](./OFL-MPLUS1Code.txt)。HTMLへ埋め込む際にも著作権表示とライセンスを同梱する。

## meerkat-companion.webp（第3版）

ユーザーの参照画像に描かれたミーアキャットを方向性の参考とし、2026-09-07にImageGenで新たに生成した。砂色の胴体、アイボリーの腹、茶色の手描き輪郭で統一。空の状態に限らず、通常ホーム・サーバー・設定・デスクトップのレビュー表示に使う。

透過を依頼した最初の2候補は、チェッカー模様がRGBに描かれていたため採用していない。最終画像は白背景で生成し、960×960のWebPへ縮小した。画像ファイル自体はRGBであり、透過画像ではない。ライトテーマはCSSのmultiply、ダークテーマはSVGフィルターによる白マット除去で画面へ馴染ませる。フィルターはブラウザ表示時の処理で、生成画像そのものを透過PNGと称していない。

最終候補の生成プロンプト:

> Keep this exact meerkat illustration and its colors, character, slender pose, face, dark ink outline, sand tan fur and ivory belly. CHANGE ONLY THE BACKGROUND. Replace the entire gray checkerboard with perfectly solid PURE WHITE #FFFFFF. No checkerboard anywhere. No texture, no gray gradient, no drop shadow, no border. Preserve the tiny drawn ground stroke. Pure white blank background, flat clean two-dimensional editorial ink drawing. Whole animal and tail visible with generous white margin. DO NOT make a transparency pattern. Plain white background.

この編集の元になるキャラクターは、参照画像から「自然な細身のミーアキャット、落ち着いた表情、茶のインク・砂色・アイボリー、日本の文具に馴染む簡潔な手描き挿絵」という指定で生成したもの。モデルを指定する引数がない組み込みImageGenを使用し、特定モデル名での生成は確認していない。
