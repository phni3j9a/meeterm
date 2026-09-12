# Issue #17: Herdr端末経路の成立性検証

検証日: 2026-09-12。**Issue #17は未完了、Herdr接続機能は未提供です。**

第一段階で指定された実接続検証を行い、Herdrの描画用ANSIを既存Rust端末へそのまま渡すだけでは
特殊キーの入力互換性を満たさないことを再現しました。tmuxの本番接続経路は変更していません。
今後の設計と未完了項目は[HERDR.md](../HERDR.md)に記載しています。

## 対象と検証境界

- Linux x86_64、ユーザー権限の隔離OpenSSH fixture。
- インストール済みHerdr `0.9.0`、API schema version `1`、protocol `22`。
- 公式[`v0.9.0`](https://github.com/herdrdev/herdr/releases/tag/v0.9.0)、source commit
  [`b99002ac99b09e00b4ca692436cb15a6b0d676f1`](https://github.com/herdrdev/herdr/commit/b99002ac99b09e00b4ca692436cb15a6b0d676f1)。
- デフォルトとnamed sessionを専用XDG config/state/cache以下に作成。既存sessionは対象にしない。
- SSH host keyをfixture生成鍵で固定し、`StrictHostKeyChecking=yes`で接続。
- SSH exec経由で取得したframeをRustの実`Terminal`へ渡し、native snapshotとproduction入力経路を検証。
- 追加依存、モバイル本番コード、SSH認証処理の変更なし。テスト専用Rust moduleは`cfg(test)`のみ。

公式の[CLI reference](https://herdr.dev/docs/cli-reference/#direct-terminal-attach)と
[Socket API](https://herdr.dev/docs/socket-api/)を確認した後、対象tagの実装と実接続で確かめています。
「0.9.0以降をサポートする」という最低バージョンの宣言ではありません。

## 再現

Herdr、Rust toolchain、Python、OpenSSH server/client、tmuxが既に利用可能な環境で実行します。
このコマンドはインストール、sudo、ユーザーのサーバー設定変更を行いません。

```sh
python3 scripts/herdr/feasibility.py --output /tmp/meeterm-herdr-check
```

`--output`は存在しないディレクトリを指定します。標準出力に各確認のPASS/FAIL、
成果物に`report.json`、合成データのframe、native snapshot、生成した入力bytesを残します。
テスト用SSH秘密鍵、raw sshd/Herdrログは成果物へコピーせず、fixtureとともに削除します。
native helperのビルドはリモート入力の待機開始前に行います。

入力契約を満たさないチェックがあれば**終了コード1**です。既知のHerdr不一致をxfailやskipに変えて
成立性ゲートを緑にしません。Rustのreplay helperだけが成功しても、Herdrの入力受け入れ成功ではありません。

## 実測結果

機械可読の[実測レポート](issue-17-herdr-report.json)を保存しています。
実行時刻、対象バイナリと検証ソースのSHA-256、各確認結果を含みます。
通常シェルと全画面TUIの画面を共有Rust端末へ再生し、DECCKMを設定したTUIで次の差分を確認しました。

| 経路 | native Up | リモート受信 | 判定 |
| --- | --- | --- | --- |
| rawのモード設定をRustへ直接渡す対照実験 | `1b4f41` | 対象外 | 既存native encoderは正しい |
| Herdr描画frame → Rust → `terminal.input` | `1b5b41` | `1b5b41` | 必要な`1b4f41`と不一致 |
| 別CLIの`pane.send_keys up` | 対象外 | `1b4f41` | 符号化できるがcontroller leaseの確認を迂回 |

frameを受けたnative pasteは`first\rsecond`で、bracketed pasteのラッパーがありません。
これは通常のraw出力ではmodeを受け取る既存native経路へ、モードのない描画用frameを与えた結果です。
同じcontrol streamに完全なbracketed pasteを一括で渡す適応では、TUIがラッパーを含めた正しいbytesを受信しました。
貼り付け自体はupstreamの未解決点ではありません。

計11確認のうち9件が成功、既存入力経路へ単にframeを与える場合の特殊キーとpaste mode復元の2件が不一致でした。
通常シェル、日本語のnativeセル、実PTYの40×16から52×20へのresize、全画面TUIのnative snapshot、
二つ目の直接controllerのtakeoverなし拒否、明示的paste、通信断後の動作中TUIへの復帰、通常release後の
同じterminalへの再接続を確認しました。全体の終了コードは1であり、Herdr対応の合格記録ではありません。

PC clientが一度も接続していないheadless sessionでは、SSH切断後も直前の20行が残りました。
この値をデスクトップの復帰失敗と決めつけず、通常Herdr clientでの引き継ぎ検証と区別します。
新しい直接controllerは一度の接続で取得でき、同じ全画面プロセスの状態をfull frameから再生できました。

検証用readerはANSI内の文字列検索を使いません。描画差分では`DECCKM_READY`が複数のcursor移動で分割されることを
観測したため、各frameを既存Rust `Term`へ適用したsnapshot上で完了を判定します。
native helperの単独実行が0件のテストで終了した場合も、freshな結果ファイルがなければ成功にしません。

共有Rustの通常テストは57件成功、Herdr replay helperと既存実SSHテストは明示実行のため標準実行ではignoredです。
Herdrの実接続は上のコマンドで別途実行しました。`cargo fmt`、`cargo clippy --all-targets -- -D warnings`、
検証readerのPython回帰6件も成功しています。

## 原因と代替経路

直接CLIの[frame出力](https://github.com/herdrdev/herdr/blob/b99002ac99b09e00b4ca692436cb15a6b0d676f1/src/client/terminal_sessions.rs#L122-L156)は
描画ANSIをBase64化したものです。入力モードをそのまま転送するPTYのraw streamではありません。
[直接入力の処理](https://github.com/herdrdev/herdr/blob/b99002ac99b09e00b4ca692436cb15a6b0d676f1/src/server/pane_input.rs#L187-L201)は
完全なbracketed paste以外をraw bytesとして送信します。

[`pane.send_keys`](https://github.com/herdrdev/herdr/blob/b99002ac99b09e00b4ca692436cb15a6b0d676f1/src/app/api/panes.rs#L1917-L1939)は
リモートruntimeの論理key encoderを利用しますが、direct controller ownerを確認しません。
現controllerが有効な間にも別automation接続から入力できることを実測し、所有権を保つ対話入力経路の代替には採用していません。

private protocolを直接実装する案も検討しました。Kitty/modifyOtherKeysの通知はありますが、
直接NDJSON wrapperはそれを転送せず、さらにDECCKM等の完全なモード情報にはなっていません。
今回のために非公開wire protocolのクライアント全体を再実装する方針にはしていません。

最小の提案は、同じ直接controller接続に論理キー入力を追加し、既存のリモートencoderとowner checkを使うことです。
meeterm専用daemon、WebView、別APIからの所有権を迂回する入力へ切り替えません。

## 未検証と後片付け

このPoCはLinuxの共有Rust snapshotまでです。Android/iOSのnative UI変更はなく、Mobile smokeや画像確認は
今回のPoCに対して実施していません。既存のMobile成功をHerdr対応成功へ読み替えません。
実機IME、GPU、フォントfallback、scrollback、端末応答、イベント購読、全CRUD操作、
複数Workspace/Tab/Pane切り替え、通常PC clientとの競合と引き継ぎの全条件は未完了です。

試験のsession/processとSSH鍵はfixture所有物だけを停止・削除します。Herdrバイナリの導入・更新、
既存serverの停止・再起動、既存接続の設定移行は行っていません。
