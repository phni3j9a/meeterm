# Issue #17: 既存Herdrの公開入力機能による再検証

これはアプリ統合前の診断記録です。以下の「未実装・未完了」は当時の状態を表します。
現在の実装と制約は[HERDR.md](../HERDR.md)、その後の結果は
[native統合検証](issue-17-herdr-native.md)と[モバイル受入記録](issue-17-herdr-mobile.md)を参照してください。
当時の失敗・測定値は保存しています。

2026-09-12、Herdr **0.9.0 / protocol 22**を変更せず、公開CLIの論理キー入力で通常モードと
application cursor modeの両方に正しいUpが届くことを実接続で確認しました。
「Herdr本体へのAPI追加が必要」という以前の結論は誤りでした。

Issue #17のアプリ実装は未完了です。この記録はPythonによる逐次実行の診断であり、
meetermのRust入力actor、React Native画面、Android/iOSのHerdr接続を実装・検証したものではありません。

## 再現と結果

```sh
python3 scripts/herdr/public_input.py --output /tmp/meeterm-herdr-public-check
```

出力先は新しいディレクトリを指定します。SSHホスト鍵を固定したユーザー権限のOpenSSH fixtureと、
専用XDG環境のHerdrを使います。利用者の既存session、Herdr本体、SSH設定は変更しません。
画面はSSHで受信したframeを既存Rust `Terminal`へ適用し、native snapshot上で確認します。
判定用の文字列がANSI描画差分で分割されても、raw文字列検索へ置き換えません。

[実測レポート](issue-17-herdr-public-input-report.json)の10確認はすべて成功し、終了コードは0でした。
実行時刻、検証ソースとHerdrバイナリのSHA-256、期待値とリモート受信値を記録しています。

| 確認 | 経路と実測結果 |
| --- | --- |
| 初回描画 | 直接controller取得後にfull frameを受信 |
| 通常モードのUp | `pane send-keys <pane-id> up` → `1b5b41` |
| application cursor modeのUp | 同じ公開CLI → `1b4f41` |
| 改行と日本語の貼り付け | 元の`first\n日本語`を一つの完全なbracketed pasteとしてdirect streamへ送り、LF・UTF-8・ラッパーまで一致 |
| 別Workspaceへのフォーカス変更 | 新しいWorkspaceのIDと現在フォーカスの一致をsnapshotで確認 |
| 明示したPaneへの文字入力 | フォーカスを変えた後も、元のPane内のTUIが`TARGET_OK`を受信 |
| 無断takeoverの禁止 | 二つ目のcontrollerが拒否されたことを確認 |
| 明示的な競合操作の通知 | fixture側の別controllerが意図的にtakeoverすると、旧streamへ終了理由が届く |
| 終了通知後の送信停止 | 通知を読んだ診断clientを無効化し、以後のCLI呼び出し回数が増えないことを確認 |
| 解放と再接続 | 同じterminal IDの動作中の全画面TUIをfull frameから再表示 |

各入力は対応するモード設定後の画面マーカーを確認してから送ります。TUIはraw modeとalternate
screenを使用し、期待bytesを専用ファイルへ記録します。再接続後はTUIを通常終了させ、shellに戻った
完了マーカーまで確認してからfixtureを片付けます。

## 前の診断との違い

[前の診断](issue-17-herdr-feasibility.md)は、描画frameから端末の入力モードも復元できると仮定して、
meetermの既存byte encoderをそのまま使う候補でした。そこで観測した2件の不一致は変更していません。
今回の診断はキーを論理名で既存Herdrへ渡す別経路です。Herdrが自分で保持する現在モードに応じて
符号化するため、描画frameからモードを推測する必要がありません。

公開CLIは各呼び出しでHerdrの公開APIを使用し、応答を待ちます。今回のキー入力は`pane.send_keys`、
確定文字列は`pane.send_text`に対応します。貼り付けは実測済みのdirect streamを使用しています。
`pane.send_input`の貼り付けや、複数入力経路を用いた本番actorの順序制御を合格したとは記録しません。

## 残る境界

- 診断clientの停止確認は、終了通知を読み終わった後の新規送信が対象です。非同期のRust actorは未実装で、
  takeover時に送信済みのAPI要求を取り消せるという証拠ではありません。
- direct controllerの排他は通常のHerdr PC画面やautomation全体を排他しません。同時編集の完全保証は
  Issueの対象外ですが、競合の表示と操作権喪失後の入力停止は本番実装でも必要です。
- 切り替え中の入力順序、購読と再同期、通常PC画面での入力再開、全操作、scrollback、両OSの統合は残っています。
- 初期案のSSH Unix socket forwardingは、このユーザー権限のOpenSSH fixtureで拒否されました。
  [OpenSSH 9.6の処理](https://github.com/openssh/openssh-portable/blob/V_9_6_P1/serverloop.c#L467-L469)には
  対象UIDまたはprivilege separationに関する条件があります。今回の成功は既存CLIをSSH execで呼ぶ経路であり、
  Socket APIのSSH転送が成功したという意味ではありません。sudoやsystem sshdの変更は行っていません。

通常CIと既存Mobile smokeの結果は[前の記録](issue-17-herdr-feasibility.md#未検証と後片付け)を参照してください。
Android fullのforeground復帰後の入力確認失敗は、今回の10確認で解消したことにはなりません。
