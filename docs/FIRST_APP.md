# 初版の実用評価

HTMLモック第5版の画面を、既存のReact Native / Expo・共有Rust・
ネイティブ端末・SSH / tmuxへ接続した初版です。現在、両OSの受け入れ検証を進めています。
**下の検証記録が埋まるまでは、初版の受け入れ完了を意味しません。**

## 初版の操作範囲

- SSHホスト・ポート・ユーザー・OpenSSH秘密鍵・必要ならパスフレーズを入力。
- 初回ホスト鍵のSHA-256指紋を明示的に承認。承認済み鍵の変更は拒否。
- 接続先の実際のtmux windowをワークスペース一覧として選択・検索。
- 実際のtmux paneをターミナルタブで選択し、ネイティブ端末で表示・入力。
- 接続状態・エラーを表示し、明示的な切断・再接続を提供。
- 戻る操作やpane切り替えでリモートプロセスを破棄しない。
- PCへの引き継ぎはスマホを切断して `tmux attach -t meeterm`。

接続先プロファイルや秘密鍵は保存しません。再接続に必要な解析済みの鍵は
Rustのプロセスメモリだけに保持します。アプリの終了後は再入力が必要です。
承認済みホスト鍵の保存・照合は既存のネイティブ実装を維持します。

作成・名前変更・削除、広範な設定、複数接続先の永続的な管理は初版の必須導線に
含めず、未実装のボタンを表示しません。必要なwindow / paneは通常のtmuxから
作成できます。デモ画面は通常の起動・接続画面に混在させません。

## モックから初版への操作対応

| モックの操作 | 初版での扱い |
| --- | --- |
| ワークスペース一覧・検索・picker | 実tmux windowを対象に提供 |
| ターミナルタブ | 実pane IDを対象に提供 |
| サーバー追加・切り替え | 1接続の情報入力・接続状態シートへ整理。複数プロファイル保存は対象外 |
| window / pane追加、名前変更、削除 | 未実装の操作を表示せず、通常のtmuxで行う |
| フォントサイズ・テーマ設定 | 設定画面を非表示。端末は固定メトリクス、ホームはOSのテーマに追従 |
| 汎用Ctrlトグル | 初版は動作する専用Ctrl-Cキーを提供 |
| キーボード表示切り替え | 端末面をタップして表示。非表示はOS操作、iOSは補助キーにも用意 |
| PC引き継ぎコマンドのコピー | 選択可能なネイティブTextを長押ししてコピー |
| プレビューの状態切り替え | 通常アプリから除外。接続状態はRustの実状態を表示 |

## 接続先の準備と使い方

接続先は通常のOpenSSHサーバーとtmuxが必要です。現在の認証方式は
OpenSSH秘密鍵による公開鍵認証です。サーバー側に対応する公開鍵を登録し、
SSH経由のシェルから `tmux` を実行できるようにしてください。
アプリ専用サーバーやソケットは不要です。

秘密鍵欄には `-----BEGIN OPENSSH PRIVATE KEY-----` から
`-----END OPENSSH PRIVATE KEY-----` までの全文を貼り付けます。
公開鍵（`.pub`）や旧PEM形式の秘密鍵は受け付けません。新しく用意する場合は、
PCで未使用の保存先を指定して `ssh-keygen -t ed25519 -f ~/.ssh/meeterm_ed25519`
を実行し、生成された `.pub` の内容をサーバーの対象ユーザーに登録します。
アプリへ入力するのは拡張子のない秘密鍵です。

ホスト鍵の照合では、すでに信頼できる管理経路からサーバー上で、例えば
`ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub -E sha256` を実行します。
アプリが表示する鍵アルゴリズムに対応する公開ホスト鍵ファイルを選び、
SHA-256指紋を比較してください。未確認の接続先から `ssh-keyscan` で取得した
指紋だけを信頼の根拠にはしません。
コマンドの詳細は[OpenSSHのssh-keygenマニュアル](https://man.openbsd.org/ssh-keygen.1)を参照してください。

1. アプリの接続ボタンから接続情報を入力します。初版では毎回入力します。
2. ホスト鍵の指紋を信頼できる別の経路で確認して承認します。
3. 接続後、ワークスペースを開いてpaneタブを選びます。
4. 端末面をタップしてOSキーボードを開きます。Esc・Tab・矢印・Ctrl-Cは
   ネイティブ補助キーから送信します。履歴は端末面の上下スワイプで読み、
   入力すると最新出力へ戻ります。貼り付けは端末の **Paste** 操作を使います。
   OSキーボード独自のクリップボード機能は通常の文字確定として届く場合があるため、
   bracketed pasteを保証しません。
5. ワークスペース一覧へ戻っても接続とリモート作業は継続します。
6. 作業終了時は切断します。再接続は同じリモートtmuxへ戻ります。
7. PCでは同じユーザーでSSH接続して `tmux attach -t meeterm` を実行します。

接続時に `meeterm` セッションがなければ自動作成されます。追加の作業を作る場合は、
同じサーバー・ユーザーで通常のtmuxを操作します。例えば次の操作は `project`
というワークスペースと、その中の2つ目のターミナルを作ります。

```sh
tmux new-window -t meeterm -n project
tmux split-window -h -t meeterm:project
```

アプリの一覧へ反映されます。名前変更や終了も通常のtmuxから行ってください。

ホスト鍵変更のエラーは自動承認しません。正当な交換かどうかを確認するまで
接続を中止してください。

## Android

開発時は[Androidの環境手順](POC_ANDROID.md)に従いSDK・NDK・JDKを用意します。
生成済みの `android/` は編集元ではありません。

```sh
npm ci
npx expo prebuild --platform android --non-interactive --no-install
npx expo run:android --device
```

このDevelopment BuildはMetroを使用します。SSHで開発マシンを操作する場合は
USBで `adb reverse tcp:8081 tcp:8081` を設定するなど、端末からMetroへ到達できる
状態にします。Expo Goでは動作しません。

自己完結する評価APKはMobile smokeの `android-emulator-observability` に
`app-release.apk` として保存する構成です。arm64実機とx86_64エミュレーターを
対象にし、JavaScriptを同梱するためMetro不要です。開発用の署名を使用し、
ストア配布用の署名・公開は今回の範囲外です。

```sh
adb install -r app-release.apk
adb shell monkey -p dev.meeterm.app 1
```

Androidの実SSH操作が通過した評価APKは
[run 34208501053 の成果物](https://github.com/phni3j9a/meeterm/actions/runs/34208501053/artifacts/10049615650)
から取得できます（commit `652ad58`）。iOSの受け入れ検証は継続中です。

```sh
gh run download 34208501053 --repo phni3j9a/meeterm \
  --name android-emulator-observability --dir artifacts/android-evaluation
adb install -r artifacts/android-evaluation/app-release.apk
adb shell monkey -p dev.meeterm.app 1
```

このAPKのSHA-256は
`3251589d0487eafecfa1a2bc3a1e483afa8fe391f875cc362dc80e740878d166`
です。JavaScript bundleとarm64 / x86_64の共有Rustライブラリの同梱を確認しています。

## iOS

macOS・Xcode・対応するSimulator runtime・CocoaPods・Node・Rustが必要です。
Intel MacのSimulatorでは `x86_64-apple-ios`、Apple Siliconでは
`aarch64-apple-ios-sim` のRust targetを使用します。

```sh
npm ci
rustup target add aarch64-apple-ios-sim  # Apple SiliconのSimulator
# Intel Macでは上の行の代わりに: rustup target add x86_64-apple-ios
npx expo prebuild --platform ios --non-interactive --no-install
cd ios
pod install
cd ..
npx expo run:ios
```

Development BuildはMetroが必要です。CIはRelease構成でJSを同梱し、
署名なしのSimulatorビルド・インストール・起動を検証します。
iPhone実機にはAppleの開発署名、開発チーム設定、対象端末の開発者モード、
`aarch64-apple-ios` targetなどが別途必要です。TestFlightは今回の対象外です。
実機用targetは `rustup target add aarch64-apple-ios` で追加します。
生成したXcodeプロジェクトの開発チーム・署名設定と端末の準備を済ませてから、
`npx expo run:ios --device` で接続したiPhoneを選びます。

Hosted SimulatorでのCoreGraphics fallbackはMetal実行の証拠ではありません。
Simulatorで成功しても実機GPU・日本語IME・フォントフォールバックの同等性は
証明しません。

## 作業中の検証記録

| 検証 | 現在の証拠 |
| --- | --- |
| 共有Rust | 42 unit tests、Clippy通過。2026-09-08ローカル |
| OpenSSH＋tmux | 隔離fixtureで接続・鍵確認・pane入出力・サイズ変更・切断・再接続・PC attach・pane消失・Ctrl-C・既存のPC zoom保持・window内active pane保持通過。通常同期中のReady維持、画面再取得後の入力可能状態保持も回帰確認。2026-09-08ローカル |
| UI型チェック | 新UI統合後のTypeScriptチェック通過。2026-09-08ローカル |
| Android CI | [652ad58 / run 34208501053](https://github.com/phni3j9a/meeterm/actions/runs/34208501053)のAndroid job成功。実SSH接続・window/pane切り替え・入力・切断・再接続・同一shellへの入力・最後の切断後split/zoom復元まで通過。入力拒否0件 |
| iOS CI・実SSH操作 | 8070c01で実SSH接続・workspace/pane切り替え・標準Paste処理完了まで進行。Return検索とUIKitの非同期待機を修正した[652ad58 / run 34208501053](https://github.com/phni3j9a/meeterm/actions/runs/34208501053)は署名なしbuild成功だが、テスト開始記録が作られる前にUIテストが失敗し、画像も未取得。テストランナーの固定カテゴリ診断を追加して再検証中。リモート入力確認・切断・再接続・最終native gateは未完了 |
| 両OSスクリーンショット目視 | Android 652ad58の5枚（実workspace、端末＋キーボード、再接続後端末、PCヘルプ、native foundation）とiOS 8070c01の6枚（空の接続フォーム＋キーボード、ホスト鍵確認、workspace一覧、workspace切り替え、pane切り替え、端末＋キーボード）をダウンロードして目視済み。iOSの補助キーは横スクロール＋常時表示の閉じるキー。自動大文字化の解消を確認。8070c01のネイティブログはMetal first frameを4件記録しているが、最終native gateの完了を意味しない |
| Pixel 3実機 | arm64 Releaseのbuild/install/launch、初回ホーム・接続フォーム・Gboard表示を目視。ネイティブJVM11テスト通過。実SSH UIは未完了。別アプリが前面に出たためユーザーの端末利用状況を確認中 |
| iPhone実機・実機Metal・実日本語IME | 未検証 |
| APK・PR | [Draft PR #12](https://github.com/phni3j9a/meeterm/pull/12)、Android評価APKは上記成果物から取得可能。両OSの受け入れ完了は未達 |

## 既知の検証境界

Androidの絵文字は単色glyphで描画し、カラー表示と全種類のcoverageは未対応です。
cell幅と配置の確認範囲は[Android端末の検証記録](POC_ANDROID.md)を参照してください。

自動再接続は追加しません。アプリ終了後の復帰は接続情報の再入力が必要です。
任意の全画面TUIのプロセス終了後の完全復元は未保証であり、再描画が必要な
場合があります。詳細は[SSHの復元境界](SSH.md)を参照してください。
