# SSHパスワード認証とFold7への導入

アプリの実装対象は `ba31908`、iOS操作テストの修正対象は `4ab4365`
（2026-09-09 JST）です。

## 変更

接続フォームで「秘密鍵」「パスワード」を選択できます。秘密鍵を初期値とし、
パスワードを選んだ場合はユーザー名とSSHパスワードで認証します。
SSHの `password` メソッドに対応し、keyboard-interactive、MFA、SSH agentは
今回の追加範囲に含めません。

パスワードの前後の空白を削除しません。フォームの秘密情報は送信、キャンセル、
認証方式の変更時に消去します。Rustは明示的な再接続に必要なパスワードを
`Arc<Zeroizing<String>>` でプロセス内に保持し、ディスクに保存しません。
初回のホスト鍵承認と、変更されたホスト鍵の拒否は公開鍵認証と共通です。

新しい接続を受け付けた時点で、以前の再接続用認証情報を無効化します。
非秘密の接続先情報を別に保持し、新しい秘密鍵の読み込みが失敗した場合でも、
同じ接続先のpane情報を維持し、別の接続先では古いpane情報を破棄します。

Androidではキーボード表示時にフォームの高さを調整し、フォーカス中の
パスワード欄を見える位置へスクロールします。

## 検証

| 検証 | 結果 |
| --- | --- |
| Rust unit tests | 44件成功。認証方式の不正・混在と、認証情報変更失敗後の再接続・接続先変更の回帰を含む |
| Rust format / Clippy | 成功 |
| Android Rust targets | `aarch64-linux-android` と `x86_64-linux-android` のcheck成功 |
| Android native unit tests | Debug / Release各16件成功。認証オプションの5件を含む |
| TypeScript | typecheck成功 |
| SSH smoke driver | Python回帰64件成功 |
| 実OpenSSHパスワード認証 | 下記の統合テスト1件成功、3.48秒 |
| 独立コードレビュー | 指摘を修正し、未解決の重大な指摘なし |
| 一般CI | `f502e0a` の[run 34341016988](https://github.com/phni3j9a/meeterm/actions/runs/34341016988)全ジョブ成功 |
| Android Hosted | `ba31908` と `f502e0a` の[run 34341016983](https://github.com/phni3j9a/meeterm/actions/runs/34341016983)で成功。両方の画像を実際に開いて確認 |
| iOS Hosted | `4ab4365` の[run 34343699940](https://github.com/phni3j9a/meeterm/actions/runs/34343699940)で再検証中 |

パスワード対応の使い捨てDocker OpenSSH＋tmux環境で、次のテストを実行しました。
ユーザーのSSH設定、ログインパスワード、通常のtmuxサーバーは変更していません。

```sh
cargo test --locked --manifest-path native/meeterm-core/Cargo.toml \
  --test openssh real_openssh_password_auth_reconnect_and_host_key_gate \
  -- --ignored --nocapture
```

前後空白を含むパスワードによる接続、誤パスワードの拒否、端末入出力、
切断後の再接続、pane / native terminal IDの維持、変更されたホスト鍵の拒否を
確認します。接続先・パスワード・検証済みホスト鍵は権限0600のfixture環境
ファイルから渡しました。秘密値はログと成果物に含めていません。
検証後に一時コンテナと認証情報の一時ディレクトリを削除しました。

標準CIの既存OpenSSH fixtureは公開鍵認証用です。このパスワード統合テストは
別のfixtureを必要とするため、通常のCIでは既存の公開鍵テストを名前で指定します。

### モバイルで確認した範囲

`ba31908` のAndroid `password-form-keyboard.png` を目視し、入力欄と接続ヘッダーが
キーボードの上で見えることを確認しました。`password-form.png` と
`ssh-terminal-keyboard.png` も目視しました。キーボードを閉じた直後の画像は
レイアウト復帰途中の可能性があるため、安定した非表示状態の証拠とは扱いません。
`f502e0a` の3枚も目視しましたが、password-form-keyboard画像はIME表示・レイアウト
変更の途中で、キーボードそのものがまだ写っていません。同コミットのアプリ本体は
`ba31908` と同一ですが、この画像単体で入力中の可視性を再確認したとは扱いません。

Androidの既存の秘密鍵フォームからの実SSH接続、ホスト鍵承認、window/pane選択、
端末入力、切断・再接続、通常のPC attach、native readiness/first frame/no-crashが
通過しています。これはAndroidフォームから実パスワードを送信した検証ではありません。

最初の `e441b06` ではiOSブリッジの余分な閉じ括弧によりコンパイルが失敗し、
`ba31908` で修正しました。その後はビルドに成功しましたが、追加したiOS操作テストで
パスワード方式が押せず失敗しました。取得した接続フォーム画像を目視すると認証方式が
IMEの背後にあり、全面スワイプの開始位置がIMEに入る可能性が高いため、`f502e0a` で
対象フォームと可視領域に限定したドラッグへ変更しました。
このテストに追加した診断用 `NSStringFromCGRect` が現在のSwiftで使用不可のため、
`f502e0a` のテストビルドは失敗しました。`4ab4365` でSwiftの
`String(describing:)` に修正して再検証しています。iOSのパスワード画面や
最終native gateの成功は、まだ確認できていません。
`4ab4365` の最初のiOS実行は `index.crates.io` のDNS解決タイムアウトで
依存ライブラリを取得できず失敗しました。同コミットの一般CIでは既存の
通常tmux attach後の切断確認がタイムアウトしました。失敗ジョブを再実行しています。
通常tmux attachのテストは別のPR CIでも同じタイムアウトを再現したため、固定250msの
待機を廃止し、通常clientが接続されて正の端末サイズを持つことを確認してから
Ctrl-b dを送るよう修正しました。切断後のchild正常終了検証は維持しています。
この修正は製品コードに影響せず、実OpenSSH統合テスト1件（10.37秒）、Clippy、
独立レビューを通過しています。

## Fold7への導入状況

Galaxy Z Fold7（`SM-F966Z`、arm64）へ最初のパスワード対応版 `e441b06` を
`adb install -r` で更新インストールし、新しいフォームの実機画像を確認しました。
そのAPKのSHA-256は
`26d4725edbd160479cdb5c82c03d7df116adaab1f4f6f07ec784ecc32527055b` です。

キーボード表示修正を含む `ba31908` のローカルarm64 Releaseビルドも成功しました。
SHA-256は `fc41149d791fc13a86a1786f8c4b0ef7a9906c7a8a55b827e22d8ce0bd05f0df` です。
この最終修正版への更新中にADB接続が切れたため、導入はまだ確認できていません。
ワイヤレスデバッグの現在の接続先を確認して継続します。

ローカルビルドはJDK 17、NDK 27.1.12297006、Rust 1.96.0、Node 22.23.2を使用。
Hosted CIのNode固定値22.22.2とはパッチバージョンが異なるため、APKは区別します。
Expo CNGから生成し、JavaScriptとarm64のRustライブラリを同梱したMetro不要のビルドです。

Fold7では接続情報の手入力が見えたため、自動フォーム操作を中断しました。
この実機を通したパスワード接続成功は未確認です。日本語IME・折り畳み時の描画の
追加検証も、Rust統合テストやエミュレーターの結果では代替しません。

## Hosted APK

[Android成果物](https://github.com/phni3j9a/meeterm/actions/runs/34337594548/artifacts/10099163134)
に `ba31908` のarm64/x86_64 APKがあります。SHA-256は
`5b1ef1852dc2e19ac0b0ef5398640d9aefd83bf904057f7e864a9537d49fa858` です。
取得・導入手順は[初版の実用評価](../FIRST_APP.md#android)に記載しています。
