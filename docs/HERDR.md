# Herdr対応の設計と成立性ゲート（Issue #17）

**Herdrバックエンドは未提供です。** 現在のアプリは従来のSSH + tmuxを使います。
[Issue #17](https://github.com/phni3j9a/meeterm/issues/17)で合意した追加バックエンドの実装に先立ち、
実Herdrのライブ端末経路を検証しています。設計やフレーム表示だけでIssueを完了にしません。
検証結果と再現コマンドは[成立性の記録](evidence/issue-17-herdr-feasibility.md)を参照してください。
その後の[公開入力機能による再検証](evidence/issue-17-herdr-public-input.md)では、Herdrを変更せずに
特殊キー・日本語の貼り付け・明示したPaneへの入力・競合通知を確認しました。本番バックエンドの実装は残っています。

Herdrは既存の外部アプリです。**変更するのはmeeterm側だけ**とし、既存の公開機能へ適応します。
Herdr本体の変更・fork・新APIの追加をIssueの前提にしません。最初の候補経路で見つかった不一致と、
既存機能全体で実現不可能だという判断は区別します。

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
直接controllerの所有権を検証しません。別経路を使う場合は、明示したPaneへの入力、競合の表示、
操作権喪失後の送信停止をmeeterm側でどう扱えるか確認が必要です。現時点では代替経路の採否は未確定です。
Issueが求める無断takeoverの禁止と、全入力を単一leaseで原子的に処理する新たな条件は区別します。

貼り付けもnativeのmodeだけに任せるとラッパーが付きません。ただしHerdrは一つの`terminal.input`に入った
完全なbracketed pasteを認識するため、貼り付けはnative側で明示的に区別して一括送信する適応が可能です。
元のUTF-8文字列とLFを保ったままラッパーを付け、モードなし入力経路でLFをCRへ変換した後のbytesは使いません。
特殊キーの問題と、貼り付けの適応で解消できる問題を混同しません。

既存の論理キー入力APIと端末接続機能を組み合わせた小さな実SSH診断は成功しました。
次は、入力順序・対象Pane・競合・解放を扱う本番Rust経路と、通常PCでの入力再開を含めて検証する必要があります。
以前の「Herdrへの論理キーAPI追加が必要」という結論は撤回しました。候補経路の不一致から
外部アプリの変更を必須と判断した点が誤りであり、Herdr本体への変更・提案・公開は行っていません。
新しい診断の成功をIssue全体の受け入れへ拡大せず、検証していない最低対応バージョンも設定しません。

再検証の候補は、表示・resizeに直接制御ストリームを使い、確定文字列を`pane.send_text`、
特殊キーを`pane.send_keys`、貼り付けを`pane.send_input`へ送る方法です。公開APIは1接続につき
1要求であり、同じSocketへ続けて要求を送る方式ではありません。meeterm側で要求と応答を直列に処理します。
入力を許可するのは対象の操作権を取得した間だけとし、競合・解放・接続終了後の新規入力を止める検証が必要です。
明示的な外部takeoverと処理中のAPI要求が競合した場合、送信済みの要求までは取り消せません。
通常のHerdr PC画面もdirect controllerとの完全排他ではなく、同時編集の保証と区別します。

公式クライアントのstable client-shell endpointも調べました。semantic inputはありますが、
第三者クライアント向けの公開Socket APIより実装・検証範囲が広く、選択Paneを一時的に全サイズへ
resizeする直接制御経路とも異なります。現段階では採用せず、既存公開APIの候補を先に検証します。

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
