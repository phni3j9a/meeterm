# Issue #13: iOS実SSHとnative smoke

## 再開後に確認したこと

[Issue #13](https://github.com/phni3j9a/meeterm/issues/13)の再開時点では、
`a16b33e` のUsername読み戻しが不一致となり、実SSH操作と最後のnative gateが
未完了でした。入力の実値を保存していないため、この失敗の原因は未確定です。

`bae68ef` でHost・Port・Usernameに限定した診断を追加しました。
失敗時に空値・UTF-16文字数・期待値のprefixか・大小文字を無視した一致か・
入力欄とキーボードの存在／操作可能性を記録します。入力値そのもの、秘密鍵、
パスフレーズ、XCTestの生ログ、xcresultは公開しません。
入力の完全一致判定、最大2試行、各入力待ち時間は維持しています。

[main / run 34234009530](https://github.com/phni3j9a/meeterm/actions/runs/34234009530)と
[bae68ef / run 34235448039](https://github.com/phni3j9a/meeterm/actions/runs/34235448039)では、
実フォームからの入力、ホスト鍵確認、SSH接続、window/pane選択、ネイティブ入力、
切断・再接続・同じshellへの入力、通常のtmux attachが通過しました。
UIKitの複数行paste・rebind後の破棄・unmount後の破棄の3テストも通過しています。
`bae68ef` は短いフォーム項目をすべて最初の試行で読み戻せました。
この結果は、過去のUsername不一致の原因を特定・修正した証拠ではありません。

## 最終起動で見つかった原因と修正

上記runのジョブ全体は失敗しました。実SSH操作後の別のnative foundation確認で、
ホストの `simctl launch` に続けて `simctl openurl` を発行していましたが、
URLの配送前にSimulatorの「Open in meeterm?」確認ダイアログで停止しました。
`bae68ef` の `terminal.png` を実際に開いて確認しています。
新しい起動にはnative readiness／first frameのログがなく、CIは失敗しました。
URLの発行成功は、native terminalが表示された証拠にはなりません。

`91f4b1e` では、この別起動をXCTestへ移しました。
実SSH操作を終えたアプリを終了し、新しく起動した対象アプリに、Appleの公開API
[`XCUIApplication.open(_:)`](https://developer.apple.com/documentation/xcuiautomation/xcuiapplication/open(_:))
で明示的なfoundation URLを渡します。通常起動の画面、SSH入力、Rust、rendererの
実装は変更していません。

成功には以下をすべて要求します。

- 新しいアプリがforegroundとなり、foundationのタイトルとnative Terminalが現れる。
- XCTestが10秒間のforeground継続を観測する。
- ホストがUTCログを検証し、新しい起動の同じPIDからnative readinessと
  MetalまたはSimulator-only softwareのfirst frameが報告されている。
- first frameの後に少なくとも5秒間の生存観測が残っている。
- 実SSH側のnativeログは別起動より前の区間から検証する。

古いログ・別PID・遅すぎるfirst frame・短すぎる生存時間・曖昧なrendererを
成功根拠にしない回帰テストを追加しています。画像はXCTestが安全な画面で取得し、
collectorはその画像を保持・検査します。テスト後の任意の画面を撮り直しません。
画像の有無やpixel差分を機械的な成功条件には追加していません。

## 最終検証

作業中にExpo doctorが要求する推奨patchが変わり、`91f4b1e` の一般CIは
Expo 57.0.20と推奨57.0.21の不一致で失敗しました。`012c987` でExpoと
その必須の関連依存だけを更新しています。Expo Modules Core／JSIのiOS実装も
更新されるため、両OSのfresh CNGとHosted mobile jobsを再実行しました。
ローカルの型チェック、Expo config、Expo doctor 21/21項目は通過しました。

`012c987a3e88cee7d9a7ba885cd33182833f7f61` の
[Mobile smoke](https://github.com/phni3j9a/meeterm/actions/runs/34243185286)は両OS成功しました。
Androidは26分1秒、iOSは41分35秒です。確認日は2026-09-09 JST、
CIの実行日は2026-09-08 UTCです。以後のREADME／FIRST_APP／本記録の更新は文書のみです。

| iOSの確認 | 実際の証拠 |
| --- | --- |
| 実フォーム | `fill_host_verified`、`fill_port_verified`、`fill_username_verified`。全て最初の試行で一致し、短い項目の失敗診断は生成されなかった |
| SSHとwindow／pane | 指紋を照合して明示承認、接続、2つのworkspaceと3つのpaneを検証。実window切り替えとpane選択を通過 |
| ネイティブ入力 | OSキーボードでの文字入力、標準Paste処理完了、Return、リモート側のmarkerを確認 |
| 再接続 | 切断後に再接続し、元のpaneとshell変数を使った追加入力が通過。`ios-validation.txt` は `result=passed`／`stage=complete` |
| PC引き継ぎ | `handoff-validation.txt` は `desktop_attach=passed`／`session=meeterm`。ヘルプ表示とは別に通常のtmux attachで検証 |
| UIKit入力 | `ios-native-input-validation.txt` のmultiline／rebind／unmountがすべてpassed |
| 実SSHの描画 | PID 7254のMetal first frameが5件。`xcuitest_renderer_backend=metal` |
| 最後の新規起動 | PID 17383でnative readinessが15:52:53.396 UTC、Metal first frameが15:52:54.288 UTC。別起動のログ区間で検証 |
| 生存観測 | 15:52:55.544〜15:53:05.619 UTCの約10.075秒をforegroundで観測。first frameから観測終了まで約11.331秒あり、5秒以上の条件を満たす |
| 最終判定 | `ios-foundation-validation.txt` は `result=passed`／`reason=none`／`renderer_backend=metal`。XCTest runnerのexit codeは0 |

iOSの[observability bundle](https://github.com/phni3j9a/meeterm/actions/runs/34243185286/artifacts/10064739563)を
ダウンロードし、次の10枚を全て実際に開きました。

| 画像 | 目視した内容 |
| --- | --- |
| `connection-form-keyboard.png` | 秘密鍵入力前の空フォーム、Host／Port／UsernameとOSキーボード |
| `host-trust.png` | 初回SSH指紋の確認と明示的な承認操作 |
| `workspaces.png` | 実際の2つのworkspaceとpane数 |
| `workspace-switched.png` | 別windowのターミナル |
| `pane-switched.png` | 元のwindowの2つ目のpane |
| `terminal-keyboard.png` | ネイティブ端末、OSキーボード、横スクロール補助キーと常時表示の閉じるキー |
| `terminal-input.png` | 入力したコマンドと戻ったshell prompt。リモートmarker確認後に取得 |
| `disconnected.png` | 未接続の表示と再接続操作 |
| `reconnected.png` | 再接続後のshell変数を確認するコマンドとprompt |
| `terminal.png` | 新規起動のnative foundation。日本語／CJK、結合文字、代表的な絵文字、ANSI色の表示。以前のOS URL確認ダイアログはない |

今回の実行で使われたbackendはMetalです。Simulator-only CoreGraphics fallbackを
実行したという証拠ではなく、fallback対応自体は保持しています。

Androidのbundleをダウンロードし、`ssh-workspaces.png`、
`ssh-terminal-keyboard.png`、`ssh-terminal.png`、`ssh-handoff.png`、`terminal.png` の
5枚を実際に開きました。workspace一覧、入力後の日本語／ANSI出力とキーボード、
再接続後の端末、PCへの引き継ぎ案内、foundationの表示を確認しています。
`ssh-validation.txt` は `result=passed`／`stage=disconnect_after_resume` で、
同一shellの復帰と最後の切断後のsplit／zoom復元まで通過しました。
native readiness／first frame／process survivalも通過し、収集ログの入力拒否は0件でした。

同じcommitの[一般CI](https://github.com/phni3j9a/meeterm/actions/runs/34243185235)も成功しました。
Rust 42件、実OpenSSH／tmux統合テスト、Python回帰64件、画像collector回帰、
Android native build／module unit tests、型チェック、Expo config／doctorを含みます。

過去のUsername不一致は、再開後のmain／`bae68ef`／`012c987` の3つの完了runで
再現しませんでした。原因は未確定のままであり、今回の起動修正と混同しません。

Simulatorでの結果は、iPhone実機GPU・実機日本語IME・フォントfallbackの同等性を
証明しません。実機署名、TestFlight、ストア配布はこのIssueの対象外です。
