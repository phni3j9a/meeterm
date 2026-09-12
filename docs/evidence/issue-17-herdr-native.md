# Issue #17: 実Herdrとproduction native backendの検証

2026-09-12、production commit `e117b06f09338c299fdcb201ce0f49be18e0e4ff` に対して、
`real_herdr_native_backend_over_russh_fixture` が **1件成功、18.34秒**で完了しました。
[機械可読レポート](issue-17-herdr-native-report.json) にソースのSHA-256と検証範囲を残しています。
先行する[公開CLIの検証](issue-17-herdr-public-input.md)と、失敗を含む
[初期候補の記録](issue-17-herdr-feasibility.md)は、その時点の結果として維持します。

## 接続したもの

Herdr公式v0.9.0（protocol 22）の既存binaryを使い、一時ディレクトリ内だけにdefaultと
named sessionを起動しました。binaryのSHA-256は
`4fa1a01158dd8043da92d31b270780b0dcc10603038d9b61cac4d81ab63fb71f`です。
Herdrのソース、インストール済みbinary、ユーザーのsession、SSH設定は変更していません。

テスト用russh SSH endpointが実Herdrの公開socketとCLIへ接続し、meetermのproduction
Rust backendがそのSSH endpointを利用します。CLIだけの模擬クライアントではなく、
通常の接続・端末registry・入力・再接続処理を通しています。ただし、SSHサーバーは
OpenSSH daemonではありません。既存の実OpenSSH/tmux統合テストは別に成功を確認しています。

## 確認結果

| 対象 | 実測したこと |
| --- | --- |
| 接続先の分離 | default/namedへの接続、他ownerのhandleと他runtimeのterminal UUIDの拒否 |
| 構造と操作 | Workspace/Group/Paneの作成・改名・終了、作成時の初期Paneが1個、選択先の更新 |
| Agent | 新しく作ったPaneへのworking/blocked/unknownイベントがnative snapshotへ届く |
| shellと描画 | 入力のechoには含まれない出力マーカーをnative snapshotで確認 |
| resizeとscroll | 実PTYが52列20行へ変化、古い行へscroll後の入力で最下部へ戻る |
| TUI入力 | alternate-screen TUIでDECCKMのUpが`ESC O A`として届く |
| 貼り付け | 日本語・LFを含む貼り付けのbracketed envelopeとUTF-8 bytesが完全一致 |
| 表示切り替え | 短時間のhide/show後もcontrollerを取り直し、TUIへUpが届く |
| 競合 | 別direct controllerがtakeoverなしで拒否され、明示的な外部takeoverを検知 |
| 再接続 | 同じfullscreen TUIへ戻り、modeを保った入力が届く |
| 外部変更 | Workspace改名とPane移動後も同じnative terminalを保持し、新しいPane aliasへ入力 |
| background | 入力停止、foreground復帰、native frameと実際の入力到達 |
| 通信断 | SSHサーバーからdisconnect後に自動再接続し、実際の入力到達 |
| client状態の喪失 | 新しいconnection owner/registryで同じremote shell変数を復元 |
| PCへの引き継ぎ | 通常のHerdr clientを100×40のPTYで起動し、同じshell変数を使ったキーボード入力が到達。構造も維持 |
| スマホ側へ復帰 | PC client終了後、native controllerを再取得して入力到達 |

Groupが空の場合の選択、snapshotと購読pane setの整合、入力queue停止、backend/runtimeの
検証などは別のRust単体テストで扱います。Rust libraryは78件成功・診断用1件ignored、
Clippyのall-targetsとTypeScriptの型検査も成功しています。

## 再現

```sh
MEETERM_HERDR_INTEGRATION=1 \
MEETERM_HERDR_BINARY=/path/to/herdr-0.9.0 \
cargo test --locked --manifest-path native/meeterm-core/Cargo.toml \
  --test herdr -- --ignored --nocapture
```

テスト終了時は専用fixtureの子プロセスと一時ディレクトリだけを片付けます。
一般CIでは同じ公式binaryを`RUNNER_TEMP`に取得し、上記digestを確認して実行します。

## 証拠の限界

- 新しいconnection ownerでの復元はclient registry喪失の検証で、モバイルOSによるprocess killの実測ではありません。
- 通信断はSSHのdisconnectです。任意のpacket lossや実回線での長時間運用を再現したものではありません。
- PC操作は通常のHerdr clientへPTYからキーを送りました。物理キーボード・画面・GPUの検証ではありません。
- これは共有native backendの検証です。Android/iOSのbuild、storage/input、実画面、実機IME/GPUは別の検証範囲です。

テスト作成中、通常disconnectの期待状態、入力echoと出力の区別、外部move後に不要な
frame更新を要求していた待機条件を修正しました。最終ケースは`Disconnected`の明示確認、
echoに現れないshell出力、move後の実入力を確認します。失敗を成功へ読み替えたり、
本番のassertionを飛ばしたりしていません。
