# Herdr対応の設計と成立性ゲート（Issue #17）

**Herdrバックエンドは未提供です。** 現在のアプリは従来のSSH + tmuxを使います。
[Issue #17](https://github.com/phni3j9a/meeterm/issues/17)で合意した追加バックエンドの実装に先立ち、
実Herdrのライブ端末経路を検証しています。設計やフレーム表示だけでIssueを完了にしません。
検証結果と再現コマンドは[成立性の記録](evidence/issue-17-herdr-feasibility.md)を参照してください。

## 採用するモデル

| アプリ内の概念 | tmux | Herdr |
| --- | --- | --- |
| Connection | SSH接続先 | SSH接続先 |
| Runtime | 通常のサーバー上のsession `meeterm` | defaultまたは指定したnamed session |
| Workspace | window | workspace |
| TerminalGroup | window内の仮想Group 1個 | tab |
| Terminal | pane | pane |

tmuxの仮想Groupのためにリモートwindow/sessionを増やしません。HerdrのTabをWorkspaceへ平坦化せず、
同じWorkspaceでも別GroupのPaneを無条件に混ぜません。UIではGroupが1個なら選択UIを隠し、
複数の場合に限ってGroup選択とその中のTerminal選択を分けます。

backend指定のない保存済み接続はtmuxとして扱う設計です。今回は保存形式も接続フォームも変更していません。
Herdrの標準接続先は既存default sessionであり、接続のたびにモバイル専用sessionを作りません。
選択したバックエンド以外への自動切り替え、Herdrの自動導入・更新・再起動は行いません。

識別子はConnection・backend・Runtimeのスコープを含めて扱います。検証ではdefaultとnamed sessionの
両方に同じ公開Pane ID `w1:p1` が発生しました。表示名を操作対象にせず、公開Pane IDとローカルの
native Terminal handleを混同しません。HerdrではWorkspaceをまたぐ移動で公開Pane IDが変わるため、
再同期で対応を更新する必要があります。移動UIは今回の必須範囲ではありません。

## 検証した接続候補

```text
OpenSSH exec channel（外側のPTYなし）
  → herdr --session <session> terminal session control <pane-id>
  → terminal.frame（NDJSON / Base64の描画用ANSI）
  → Rustでdecode、frameのwidth/heightにresize
  → 既存alacritty_terminal::Term / native snapshot
  → 既存Android / iOS native renderer（統合は未実装）
```

`terminal.frame`はHerdrが描画した画面の差分です。子プロセスのraw VT出力や全scrollbackではありません。
初回・再接続・サイズ変更時のfull frameと、その後の差分を順番に扱う必要があります。
描画データ、入力、IME compositionをJSへ移しません。既存の単一Rust runtime/registryを再利用する方針です。

構造・Agent状態は`session.snapshot`と`events.subscribe`の候補があります。購読ackを受けてから
snapshotを取得し、その間のイベントを適用する方式を検討します。公式CLIにはsnapshotコマンドがありますが、
イベント購読はSocket APIです。OpenSSHのstream-local転送が利用可能かを含め、購読経路の実接続検証は残っています。
高頻度の`pane read`や全件snapshot pollingを通常のライブ端末の代替にはしません。

リモートに必要なものは通常のSSHと選択したバックエンドです。Herdr自身の通常サーバーはHerdrバックエンドの一部です。
meeterm専用daemon、gateway、HTTP/WebSocketサービス、追加の外部公開ポートは導入しません。

## 入力互換性の未解決点

Herdr **0.9.0 / protocol 22** の直接制御CLIは、`terminal.input`で渡されたbytesを原則そのままPTYへ送ります。
一方、描画用frameはDECCKMなどの子プロセスの入力モードを伝えません。画面を既存`Term`へ描画できても、
その`Term`が子プロセスの入力モードを把握したことにはなりません。

実接続検証では、DECCKMを有効にしたフルスクリーンTUIに対し、native Upの出力とリモート受信値が
`1b5b41`（`ESC [ A`）でした。必要な値は`1b4f41`（`ESC O A`）です。
同じRust端末へ元のモード設定を直接渡す対照実験では正しい値を生成します。

`pane.send_keys`にはリモート側のモードを使う論理キーの符号化があります。ただし、このautomation APIは
直接controllerの所有権を検証しません。入力経路を分けるとcontrollerを失った後の入力を同じ所有権で拒否できず、
対話入力の代替としては採用していません。

貼り付けもnativeのmodeだけに任せるとラッパーが付きません。ただしHerdrは一つの`terminal.input`に入った
完全なbracketed pasteを認識するため、貼り付けはnative側で明示的に区別して一括送信する適応が可能です。
元のUTF-8文字列とLFを保ったままラッパーを付け、モードなし入力経路でLFをCRへ変換した後のbytesは使いません。
特殊キーの問題と、貼り付けの適応で解消できる問題を混同しません。

最小の次の検討対象は、Herdrの直接制御ストリームに**同じcontroller所有権で処理する論理キー入力**を公開することです。
既存のリモートkey encoderを再利用できれば、meetermが非公開の入力モードを推測する必要を減らせます。
別案として完全な入力モード通知がありますが、描画との順序、再接続時の初期状態、Kitty/modifyOtherKeysも含めた契約が必要です。
このAPI拡張はまだHerdrに実装・提案・公開していません。検証していない最低対応バージョンも設定していません。

## 残る実装・受け入れ

Issue #17の全受け入れ条件は維持します。現時点では次が未完了です。

- 入力互換性を解消した実Herdr端末経路と、controller競合・解放・PC引き継ぎの確定。
- 共通モデル、最小限のTmuxBackend / HerdrBackend相当の分離、設定の後方互換処理。
- backend/session設定、Workspace/Group/Terminal操作、必要時だけのGroup UI。
- 外部での作成・終了・改名・移動を含む購読/再同期と、安全な選択先・空状態。
- Agentメタデータと集計。`working / blocked / done / idle / unknown`を保持し、切断やunknownを成功として表示しない。
- 複数Workspace/Group/Terminal、scrollback、端末応答、破壊的操作の影響の検証。
- Android full、iOS standard + ssh、両OS画像の実見、実機IME/CJK/GPUなどの確認。

現在のPoCはLinux上の共有Rust terminal snapshotまでです。アプリの接続UI、ネイティブGPUの画像、
Android/iOSのIME成功、PCとの完全な同時編集を示すものではありません。
