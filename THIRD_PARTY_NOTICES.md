# Third-party notices

このファイルは native terminal と SSH slice で利用・参照する third-party provenance の inventory です。各 upstream の license text、copyright、改変条件が優先されます。ここで「依存」と記載していない feasibility reference は、実行時依存でも source copy でもありません。

## Native terminal

| Component | Version / source | License and notice |
| --- | --- | --- |
| `alacritty_terminal` | `=0.26.0`（`native/meeterm-core/Cargo.lock`） | Apache-2.0 OR MIT。crate metadata と upstream notice は [crates.io](https://crates.io/crates/alacritty_terminal/0.26.0)、[Apache notice](https://github.com/alacritty/alacritty/blob/master/LICENSE-APACHE)、[MIT notice](https://github.com/alacritty/alacritty/blob/master/LICENSE-MIT) を参照。 |
| `vte` および Cargo の transitive dependencies | lockfile で解決された version | 各 crate の upstream license と notice を保持する。`Cargo.lock` を version provenance とし、リリース前に license inventory を更新する。 |

`native/meeterm-core` は上記 crate を実行時に利用します。Alacritty の license notice を meeterm の source や配布物から削除しないでください。

## SSH transport

| Component | Version / source | License and notice |
| --- | --- | --- |
| `russh` | `=0.63.2`（`native/meeterm-core/Cargo.lock`） | Apache-2.0。[crate metadata](https://crates.io/crates/russh/0.63.2) と [upstream source / license](https://github.com/warp-tech/russh) を参照。source の vendoring やコピーは行わず、Cargo dependency として利用する。 |
| `tokio` | `1.53.1`（lockfile） | MIT。[crate metadata](https://crates.io/crates/tokio/1.53.1) と [upstream license](https://github.com/tokio-rs/tokio/blob/master/LICENSE) を参照。単一の Rust runtime として利用する。 |
| `zeroize` | `=1.9.0`（`native/meeterm-core/Cargo.lock`） | Apache-2.0 OR MIT。[crate metadata](https://crates.io/crates/zeroize/1.9.0) と [upstream source / licenses](https://github.com/RustCrypto/utils/tree/master/zeroize) を参照。認証用の一時文字列を消去するために利用する。 |
| `ring` と SSH の transitive dependencies | lockfile で解決された version | `russh` の `ring` feature を使用。各 crate に含まれる license と copyright notice を保持する。`ring` は Rust / C / assembly ごとに由来が異なるため [upstream license](https://github.com/briansmith/ring/blob/main/LICENSE) の全条件を参照する。 |

## Bundled fonts

| Component | Source / status | License and notice |
| --- | --- | --- |
| M PLUS 1 Code variable font | `modules/meeterm-terminal/android/src/main/assets/fonts/MPLUS1Code[wght].ttf`（module-owned bundled asset、font version `72090`、SHA-256 `ff68678c5bd7e9d9d6ab6d57e4355aabe30f6b8f8bff9bd59baf6a7807dcfd36`、source: [M+ FONTS Project](https://github.com/coz-m/MPLUS_FONTS)） | Copyright 2021 The M+ FONTS Project Authors。SIL Open Font License 1.1。対応する license text は [module の `OFL.txt`](modules/meeterm-terminal/android/src/main/assets/fonts/OFL.txt) を参照。公式 license は [SIL OFL](https://scripts.sil.org/OFL)。 |

Font の license text は module 側で管理し、この root notice に同じ全文を重複掲載しません。asset を差し替える場合は上の inventory のファイル名と copyright を更新してください。emoji font を追加する場合は、同じ表に個別の source/license を追加します。現行の M PLUS 1 Code asset が emoji glyph を十分に含むかどうかは、実機 renderer 検証の対象です。

## JavaScript / Expo toolchain

`package-lock.json` に固定された Expo、React、React Native、`expo-dev-client`、`expo-build-properties` および transitive packages は、それぞれの npm package metadata にある upstream license と notice に従います。依存関係を再配布する際に upstream の license metadata や notice を置き換えないでください。

`modules/meeterm-terminal` の Expo Module template notice は [module の `LICENSE`](modules/meeterm-terminal/LICENSE) に保持しています（MIT、Copyright 2015-present 650 Industries, Inc.）。module source を再配布する場合も、この notice を削除しないでください。

この PoC の lockfile は license の代替ではありません。配布版を作る前に、lockfile の実際の package version と各 upstream notice を再確認してください。

## Feasibility reference（実行時依存なし）

Fressh の公開 repository は、native terminal renderer の実現可能性を調べるためだけに参照しました。

- Reference: [EthanShoeDev/fressh commit `68cdd143ca72cf8d8cb88006bb7f192db7db880c`](https://github.com/EthanShoeDev/fressh/tree/68cdd143ca72cf8d8cb88006bb7f192db7db880c)
- Fressh の source は meeterm にコピーしていません。
- Fressh の npm package、prebuilt binary、native library は meeterm の依存関係ではありません。
- Fressh の MIT 表記や upstream notice を、meeterm の third-party dependency notice として扱わないでください。

## 更新ルール

新しい native source、renderer、font、npm package、Rust crate を追加する場合は、次を同じ変更で行います。

- exact version、commit、取得元を記録する。
- upstream license と copyright notice の場所を記録する。
- 改変した vendored source があれば、その事実と元の license を残す。
- font の場合は、ファイル名、font version、copyright、license text の同梱場所を記録する。
- `docs/POC_ANDROID.md` の known limitations と renderer/font の検証結果を更新する。
