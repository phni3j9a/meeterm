# Android native terminal PoC

この文書は、dual-platform native terminal foundation における Android 側の Issue #1 vertical slice を再現するための runbook 兼、実機検証記録です。新しい検証を行った場合は、実機の機種、OS、commit、renderer/font、結果を下の記録欄へ追記してください。iOS Simulator/実機の検証契約は [`CI_MOBILE.md`](CI_MOBILE.md) に分離します。

PoC の範囲は Expo Development Build、native `TerminalView`、Rust 所有の `alacritty_terminal::Term`、固定 ANSI/VT bytes、native GPU renderer、native IME、決定的な resize です。SSH、tmux、server profile、remote backend、WebView terminal はこの段階の対象外です。

## Source of truth と CNG

React Native/Expo の source of truth は root の TypeScript と app config、native module の source は `modules/meeterm-terminal/`、端末 core の source は `native/meeterm-core/` です。

`npx expo prebuild` と `npx expo run:android` / `npx expo run:ios` が生成する `android/` と `ios/` は Expo Continuous Native Generation (CNG) の生成物です。生成ディレクトリは untracked のままにし、手で修正して動作を合わせないでください。変更は app config、local module の config/plugin、Kotlin/Swift/Objective-C/Rust source に戻します。生成ディレクトリを将来 commit する運用に変える場合は、別途 source-of-truth を明記してください。

## 固定する環境

| Component | Version | 確認方法 |
| --- | --- | --- |
| Node.js | 22.22.2 | `node --version` |
| npm | 10.9.7 | `npm --version` |
| Rust toolchain / cargo | 1.96.0 | `rustc --version`, `cargo --version` |
| cargo-ndk | 4.1.2 | `cargo ndk --version` |
| JDK | 17 | `"$JAVA_HOME/bin/java" -version` |
| Android platform | `android-36` | `sdkmanager --list` |
| Android Build Tools | 36.0.0 + Expo fallback 35.0.0 | `sdkmanager --list` |
| CMake | 3.22.1 | `sdkmanager --list` |
| Android NDK | 27.1.12297006 | `sdkmanager --list` |

JDK は 17 を明示してください。JDK 21 などが PATH の先にある状態で「たまたま」ビルドを通した結果は再現条件にしません。

この runbook の固定表は Android 開発環境を対象にします。macOS/Xcode/CocoaPods/iOS Simulator の runner と signing 境界は [`CI_MOBILE.md`](CI_MOBILE.md) の方針を使用します。

まず、実際の SDK/JDK の絶対パスを環境に合わせて設定します。

```sh
export MEETERM_ANDROID_SDK=/absolute/path/to/Android/Sdk
export ANDROID_HOME="$MEETERM_ANDROID_SDK"
export ANDROID_SDK_ROOT="$MEETERM_ANDROID_SDK"
export MEETERM_JAVA_HOME=/absolute/path/to/jdk-17
export JAVA_HOME="$MEETERM_JAVA_HOME"
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/27.1.12297006"
export ANDROID_NDK_ROOT="$ANDROID_NDK_HOME"
export PATH="$JAVA_HOME/bin:$ANDROID_HOME/platform-tools:$ANDROID_HOME/cmdline-tools/latest/bin:$PATH"
```

SDK command-line tools が配置済みであることを確認し、必要な package を明示的に入れます。

```sh
test -x "$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager"
yes | "$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager" --sdk_root="$ANDROID_HOME" --licenses
"$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager" --sdk_root="$ANDROID_HOME" --install \
  "platform-tools" \
  "platforms;android-36" \
  "build-tools;36.0.0" \
  "build-tools;35.0.0" \
  "cmake;3.22.1" \
  "ndk;27.1.12297006"

node --version
npm --version
"$JAVA_HOME/bin/java" -version
rustc --version
cargo --version
adb version
```

Node/npm と Rust toolchain は次のように揃えます。既に同じ version がある場合も、確認コマンドを省略しないでください。

```sh
nvm install 22.22.2
nvm use 22.22.2
npm install --global npm@10.9.7

rustup toolchain install 1.96.0 --profile minimal \
  --component rustfmt,clippy --no-self-update
rustup target add --toolchain 1.96.0 \
  aarch64-linux-android x86_64-linux-android
rustup default 1.96.0
cargo install cargo-ndk --version 4.1.2 --locked
```

## Build と launch

リポジトリの root で実行します。

```sh
npm ci

export RUSTUP_TOOLCHAIN=1.96.0
cargo fmt --check --manifest-path native/meeterm-core/Cargo.toml
cargo test --locked --manifest-path native/meeterm-core/Cargo.toml
cargo clippy --locked --all-targets --manifest-path native/meeterm-core/Cargo.toml -- -D warnings

(cd native/meeterm-core && \
  cargo ndk -t arm64-v8a -t x86_64 -P 24 \
    build --locked --release)

npx expo prebuild --platform android --non-interactive --no-install
npx expo run:android --device
```

`adb devices -l` で USB debugging を許可した実機が見えることを確認してから `--device` を使います。Expo CLI の device 選択で、エミュレータではなく検証対象の物理端末を選びます。Development Build を作るため、Expo Go を検証結果として扱いません。

`expo run:android` は native project が存在しない場合に prebuild、Gradle build、install、Metro 起動を行います。問題の切り分けを容易にするため、上記では prebuild を先に明示しています。

両 platform の runner 手順、artifact 命名、Codex による screenshot review、native dependency 更新時の再生成手順は [`CI_MOBILE.md`](CI_MOBILE.md) を参照してください。

## 自動検証と手動検証の境界

共有 CI で実行する下位検証は、Rust の byte/VT/registry/snapshot/input テスト、TypeScript 型検査、Expo config/doctor、各 native target の build です。Android emulator と iOS Simulator の mobile job の machine gate は、CNG 生成 project の build、install、launch、expected native module の readiness、最初の native terminal frame、crash がないことです。CI は実機 GPU、IME、フォント fallback、画面回転の parity を合格にはできません。

成功・失敗に関係なく observability bundle を常に upload します。launch 到達後は screenshot と sanitized native log を格納し、それ以前の失敗や capture failure は偽画像ではなく unavailable 診断として残します。現段階では screenshot existence や pixel-difference を machine gate にしません。native UI を変更した場合、visual success を報告する前に Codex が Android emulator と iOS Simulator の両方の実 screenshot を download して表示確認します。artifact の存在、画像サイズ、process の終了コードだけでは visual review とみなしません。

iOS Simulator は distribution signing を要求しない unsigned build/install 境界です。証明書、provisioning profile、Apple signing secret はこの job の前提にしません。iOS physical device と TestFlight は、後続の signed workflow と device-specific evidence で検証します。

生成される local Expo module の Gradle project 名は `meeterm-terminal` です。native unit test が追加された場合は `android` directory で `:meeterm-terminal:testDebugUnitTest` を実行します。CI はこの task を検出して library test を先に実行し、存在する場合だけ `:app:testDebugUnitTest` も実行します。

自動検証のローカルコマンドは次の通りです。

```sh
npm run typecheck
npx --yes expo-doctor@1.20.4

export RUSTUP_TOOLCHAIN=1.96.0
cargo fmt --check --manifest-path native/meeterm-core/Cargo.toml
cargo test --locked --manifest-path native/meeterm-core/Cargo.toml
cargo clippy --locked --all-targets --manifest-path native/meeterm-core/Cargo.toml -- -D warnings
```

## 実機チェックリスト

以下は 2026-09-01 に Pixel 3 の同じ debug build/install session で確認した結果です。自動テストだけで確認した項目は、その旨を注記しています。

### Build と terminal view

- [x] Expo Development Build が起動する（Expo Go ではない）。
- [x] custom `TerminalView` が React Native から mount される。
- [x] Android native view が `dev.meeterm.terminal.MeetermNative` の API と接続する。
- [x] terminal output、cell、snapshot、render frame が JavaScript に streaming されていない。
- [x] Rust 所有の `alacritty_terminal::Term` に固定 demo bytes が feed される。

### Rendering と terminal semantics

- [x] ASCII が表示される。
- [x] ANSI foreground/background/style が表示される。
- [x] cursor positioning が確認できる。
- [x] wrapping が確認できる。
- [x] `scrollback-history-01` から `scrollback-history-48` を投入し、viewport 外の履歴を保持する。
- [x] `日本語` などの CJK wide character が cell width を崩さず表示される。
- [x] ASCII と CJK の混在が崩れない。
- [x] combining mark（例: `é`、`が`）が意図した cell に付く。
- [x] representative emoji の cell 幅と配置を確認した。現状は単色 glyph であり、色・細部の制限は後述する。

### Input と IME

- [x] ASCII input が native view から入力される。
- [x] Escape、Tab、Enter、Backspace、Up、Down、Left、Right が明示的な terminal bytes になる。encoding は unit test、native key row は実機表示でも確認した。
- [x] Gboard 日本語入力で `きょう` を preedit し、変換候補の `今日` を commit できる。
- [x] `今日` が commit 1 回として Rust/native input path に届く。log は `nativeCount=1 byteCount=6`（内容そのものは log に出さない）。
- [x] preedit/composition が JavaScript `TextInput` または JS event stream に流れない。
- [x] commit 前の preedit が terminal input として送信されず、native overlay にのみ表示される。

### Resize と lifecycle

- [x] portrait から landscape への resize が deterministic である（`56x35` から `106x17`）。
- [x] landscape から portrait への resize が deterministic である（`106x17` から `56x35`）。
- [x] software keyboard の表示で rows/columns が更新される（portrait で `56x35` から `56x20`）。
- [x] software keyboard を閉じた後に rows/columns が `56x35` へ戻る。
- [x] compact window-size 相当を `adb shell wm size 1080x1800` で確認し、`56x28` へ更新された。終了後は `wm size reset` で物理解像度へ戻した。
- [x] view の detach/reattach 後も同じ terminal ID と Rust terminal handle が維持され、既存内容が再描画される。

## 実機結果の記録

`pass` と書く場合は、device log、screen recording、screenshot などの証跡を併記してください。下記の画像・録画は検証ホスト上で取得した実行証跡であり、アプリの source または fixture ではないためリポジトリには含めていません。検証した immutable code commit、sanitized log、artifact checksum、自動検証との対応は [Issue #1 Android device validation](evidence/issue-1-android-device.md) に固定しています。

| Date | Commit | Device / Android | ABI | Build / renderer / font | Result | Evidence / notes |
| --- | --- | --- | --- | --- | --- | --- |
| 2026-09-01 | `2eb050c` | Pixel 3 / Android 11 (API 30) | `arm64-v8a` | Expo Development Build / GLES 2.0 / M PLUS 1 Code | pass（制限あり） | final code commit の clean CNG/build、portrait、keyboard resize、`きょう` native preeditを再確認。独立レビュー前の同一branch full runで landscape `106x17`、compact `56x28`、`今日` commit `nativeCount=1 byteCount=6` も確認。証跡の区別とchecksumは [validation record](evidence/issue-1-android-device.md) を参照。emoji は単色表示。 |

## 現時点の既知の制限

- この文書は Android 側の PoC と実機記録です。iOS は同じ Rust terminal semantics に対する別の thin native adapter として、Simulator machine gate と後続の physical-device 検証で扱います。
- SSH、tmux、connection lifecycle、server profile、remote/backend transport はまだありません。
- committed input は現段階では検証のため local `Term` に loopback されます。remote shell への送信を意味しません。
- renderer は GLES 2.0 の demand-driven redraw ですが、現段階では dirty region ではなく viewport 全体を描画します。性能最適化は未実施です。
- CJK は bundled `M PLUS 1 Code` の coverage に依存します。複数 font の動的 fallback chain は未実装です。
- representative emoji は cell 幅を維持して描画されますが、alpha glyph atlas を用いた単色表示です。color emoji と全 emoji coverage は未対応です。
- view の detach/reattach は確認済みですが、Android process death 後の terminal recovery は未実装・未検証です。
- `GLSurfaceView` の context loss、長時間連続描画、端末固有 GPU/IME の組み合わせは Pixel 3 以外では未検証です。

## 参照

- [Expo: Create a debug build locally](https://docs.expo.dev/guides/local-app-development/)
- [Expo: Development builds](https://docs.expo.dev/develop/development-builds/introduction/)
- [Android `sdkmanager`](https://developer.android.com/tools/sdkmanager)
- [Android NDK/CMake configuration](https://developer.android.com/studio/projects/install-ndk)
- [Third-party notices](../THIRD_PARTY_NOTICES.md)
