# 初版の実用評価

HTMLモック第5版の画面を、既存のReact Native / Expo・共有Rust・
ネイティブ端末・SSH / tmuxへ接続した実用評価版です。
Android先行版に続き、[Issue #13の検証](evidence/issue-13-ios-acceptance.md)で
iOS Simulatorの実SSH操作と最終native smokeの受け入れも完了しました。
2026-09-09 JST（CI実行日は2026-09-08 UTC）の `012c987` で両OSのHosted jobが成功し、
両OSの画像をダウンロードして目視しています。
**iPhone実機・実機日本語IME・TestFlightの検証は別途必要です。**

## 初版の操作範囲

- SSHホスト・ポート・ユーザーを入力し、OpenSSH秘密鍵（必要ならパスフレーズ）またはSSHパスワードを選んで入力。
- 初回ホスト鍵のSHA-256指紋を明示的に承認。承認済み鍵の変更は拒否。
- 接続先の実際のtmux windowをワークスペース一覧として選択・検索。
- 実際のtmux paneをターミナルタブで選択し、ネイティブ端末で表示・入力。
- 接続状態・エラーを表示し、明示的な切断・再接続を提供。
- 戻る操作やpane切り替えでリモートプロセスを破棄しない。
- PCへの引き継ぎはスマホを切断して `tmux attach -t meeterm`。

接続先プロファイル、秘密鍵、パスフレーズ、パスワードは保存しません。再接続に必要な
認証情報はRustのプロセスメモリだけに保持します。アプリの終了後は再入力が必要です。
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

接続先は通常のOpenSSHサーバーとtmuxが必要です。認証方式はOpenSSH秘密鍵による
公開鍵認証、またはSSHパスワード認証から選べます。パスワード認証はSSHの
`password` メソッドだけを使い、keyboard-interactive、MFA、SSH-agentには対応しません。
公開鍵認証を使う場合はサーバー側に対応する公開鍵を登録し、SSH経由のシェルから
`tmux` を実行できるようにしてください。
アプリ専用サーバーやソケットは不要です。

秘密鍵欄には `-----BEGIN OPENSSH PRIVATE KEY-----` から
`-----END OPENSSH PRIVATE KEY-----` までの全文を貼り付けます。
公開鍵（`.pub`）や旧PEM形式の秘密鍵は受け付けません。新しく用意する場合は、
PCで未使用の保存先を指定して `ssh-keygen -t ed25519 -f ~/.ssh/meeterm_ed25519`
を実行し、生成された `.pub` の内容をサーバーの対象ユーザーに登録します。
アプリへ入力するのは拡張子のない秘密鍵です。

パスワード方式を使う場合は、サーバー側で SSH の `password` 認証を有効にし、
対象アカウントにパスワードを設定してください。keyboard-interactive や MFA の
追加プロンプトには対応していません。

ホスト鍵の照合では、すでに信頼できる管理経路からサーバー上で、例えば
`ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub -E sha256` を実行します。
アプリが表示する鍵アルゴリズムに対応する公開ホスト鍵ファイルを選び、
SHA-256指紋を比較してください。未確認の接続先から `ssh-keyscan` で取得した
指紋だけを信頼の根拠にはしません。
コマンドの詳細は[OpenSSHのssh-keygenマニュアル](https://man.openbsd.org/ssh-keygen.1)を参照してください。

1. アプリの接続ボタンから接続情報と、選択した認証方式の資格情報を入力します。初版では毎回入力します。
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
[run 34243185286 の成果物](https://github.com/phni3j9a/meeterm/actions/runs/34243185286/artifacts/10064096141)
から取得できます（commit `012c987`、Expo 57.0.21）。

```sh
gh run download 34243185286 --repo phni3j9a/meeterm \
  --name android-emulator-observability --dir artifacts/android-evaluation
adb install -r artifacts/android-evaluation/app-release.apk
adb shell monkey -p dev.meeterm.app 1
```

このAPKのSHA-256は
`2c41376d0f4a05404565b1189c6725afccb26dc024e7efd8a476df2657c12a13`
です。JavaScript bundleとarm64 / x86_64の共有Rustライブラリの同梱を確認しています。

## iOS

Hosted iPhone 17 Pro Simulator／Xcode 26.6で、実フォームからのSSH接続、
window／pane選択、ネイティブキーボード・Paste・Return、切断・再接続と
同じshellへの入力を確認しました。通常の `tmux attach -t meeterm` による
PC引き継ぎ、UIKit入力3テスト、新規起動のnative readiness／Metal first frame／
no-crashも通過しています。詳細は[受け入れ記録](evidence/issue-13-ios-acceptance.md)を参照してください。
以下はローカルの開発・Simulator実行手順です。

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

## 検証記録（2026-09-09 JST）

アプリ・依存・テストの検証対象は `012c987` です。以下のHosted結果は
[Mobile smoke run 34243185286](https://github.com/phni3j9a/meeterm/actions/runs/34243185286)と
[一般CI run 34243185235](https://github.com/phni3j9a/meeterm/actions/runs/34243185235)に対応します。
その後のREADME／本書／受け入れ記録の変更は文書のみです。

| 検証 | 現在の証拠 |
| --- | --- |
| 共有Rust | 42 unit tests、format／Clippy通過 |
| OpenSSH＋tmux | 隔離fixtureの実SSH統合テスト通過。接続・鍵確認・pane入出力・サイズ変更・切断・再接続・PC attach・pane消失・Ctrl-C・既存PC zoom／active pane保持を確認。通常同期中のReady維持と画面再取得後の入力可能状態も回帰確認 |
| UI・CI回帰 | TypeScript、Expo config／doctor 21項目、Python回帰64件、画像collector回帰通過 |
| Android CI | 26分1秒で成功。実SSH接続・window／pane切り替え・入力・切断・再接続・同一shellへの入力・最後の切断後split／zoom復元まで通過。native readiness／first frame／no-crash成功、収集ログの入力拒否0件。一般CIのnative module unit testsも通過 |
| iOS CI・実SSH操作 | 41分35秒で成功。Host／Port／Usernameは最初の試行で一致。ホスト鍵確認・接続・window／pane選択・ネイティブキーボード／Paste／Return・リモート入力・切断・同じshellへの再接続入力・通常のdesktop attachが通過。UIKitの複数行pasteを一度だけ配送、rebind後破棄、unmount後破棄の3テストも成功 |
| iOS最終native gate | 実SSH中のMetal first frame 5件と、最後の新規プロセスのnative readiness／Metal first frame 1件を別区間で確認。10秒以上のforeground観測が完了し、first frame後5秒以上の生存確認も通過。今回のbackendはMetalで、Simulator-only CoreGraphics fallbackではない |
| 両OSスクリーンショット目視 | 同じcommitのAndroid 5枚、iOS 10枚をダウンロードして実際に開いた。主要画面・キーボード・入力後・再接続後・native foundationを確認。iOS最後の画像は端末プレビューで、以前停止したOSのURL確認ダイアログはない。画像名と観察内容は[受け入れ記録](evidence/issue-13-ios-acceptance.md)を参照 |
| Pixel 3実機（過去の記録） | arm64 Releaseのbuild／install／launch、初回ホーム・接続フォーム・Gboard表示を目視。ネイティブJVMテスト11件通過。実SSH UIは別アプリが前面に出たため未完了。今回のHosted結果で実機確認済みとはしない |
| iPhone実機・実機Metal・実日本語IME | 未検証 |
| APK・PR | Android評価APKは上記 `012c987` の成果物から取得可能。iOSの診断と最終起動の修正・検証記録は[PR #14](https://github.com/phni3j9a/meeterm/pull/14) |

過去の `a16b33e` のUsername不一致は再開後の3つのrunでは再現せず、原因は未確定です。
失敗時の値を公開しない診断を追加し、完全一致判定・最大2試行・待ち時間は維持しました。
今回原因を確認して修正したのは、実SSHテスト後の別起動がOSのURL確認ダイアログで
停止していた問題です。経緯は[Issue #13の記録](evidence/issue-13-ios-acceptance.md)に残しています。

## 既知の検証境界

Androidの絵文字は単色glyphで描画し、カラー表示と全種類のcoverageは未対応です。
cell幅と配置の確認範囲は[Android端末の検証記録](POC_ANDROID.md)を参照してください。

自動再接続は追加しません。アプリ終了後の復帰は接続情報の再入力が必要です。
任意の全画面TUIのプロセス終了後の完全復元は未保証であり、再描画が必要な
場合があります。詳細は[SSHの復元境界](SSH.md)を参照してください。
