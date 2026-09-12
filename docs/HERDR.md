# Herdr backend

Issue #17 の Herdr backend は、meeterm の Rust/native 経路に実装済みです。
この文書は実装契約と検証範囲を記録します。Issue 全体の受入完了はまだ宣言していません。
実 Herdr のnative統合テストは成功しました。CIと両モバイルの画像確認を含む受入証拠は保留中です。
以前の失敗を含む公開 CLI の実測記録は [feasibility evidence](evidence/issue-17-herdr-feasibility.md)
に保存してあり、書き換えていません。

Herdr は既存の外部アプリです。meeterm は公開 API に適応し、Herdr 本体の変更・fork・
自動導入・更新を行いません。リモートには、SSH から実行できる既存の Herdr 0.9.0、
起動済みの対象 session、SSH stream-local forwarding の許可が必要です。meeterm 用 gateway、daemon、
HTTP/WebSocket relay、追加のリモートツールはありません。

## 接続して使う

1. PCで、使いたいHerdrのセッションを開いておきます。SSHでログインした環境からも
   `herdr --session default status --json` を実行できる必要があります。
2. meetermの接続画面でSSHの接続先・ユーザー・認証方法を入力し、「作業環境」を
   **Herdr** にします。通常はセッション名を空欄にします。named sessionを使う場合だけ、
   PCで使っている名前を入力します。
3. 初めての接続ではホスト鍵を確認します。接続後、ワークスペースを選びます。
   HerdrのTabが複数ある場合だけGroupの選択が表示され、その中のターミナルを開けます。
4. 作業を残してPCへ戻るときは **切断** を使います。PCでは同じセッションを通常の
   `herdr --session default`、または指定したセッション名で開きます。

SSHのUnix socket転送が許可されていない場合は、転送設定を確認する案内が出ます。
Herdr未導入、セッション未起動、対応機能・バージョンの不一致、入力権限の競合も
それぞれ別のエラーで案内します。以前の保存済み設定はtmuxとして読み込みます。

## 対応する公開プロトコル

現在の互換性ターゲットは **Herdr 0.9.0 / protocol 22 / schema 1** です。
これは下位互換の最低バージョン宣言ではなく、実装と integration test が照合する固定の
公開契約です。公式の [v0.9.0 release](https://github.com/herdrdev/herdr/releases/tag/v0.9.0)
と [v0.9.0 source](https://github.com/herdrdev/herdr/tree/v0.9.0) を参照します。
tag の dereferenced source commit は `b99002ac99b09e00b4ca692436cb15a6b0d676f1` です。

- `herdr --session default status --json` または指定した named session を明示的に実行し、
  protocol、session、socket を確認します。runtime 名は Herdr の session 名として扱い、
  default は `default` です。
- 接続は SSH の direct stream-local public API です。1 channel につき 1 request を順番に
  処理します。購読は subscribe ack の後に snapshot を繰り返し受け、pane set が期待値と
  一致して安定するまで snapshot を確定状態へ適用しません。その後は event と resync で
  追従します。
- terminal 表示は raw PTY を転送せず、`terminal.frame` の ANSI/VT 差分を Base64 で受けます。
  frame の `width`、`height`、`full`、`seq` を使って native `alacritty_terminal::Term` を
  更新します。frame の有無をもってアプリケーションの入力 mode を推測しません。
- direct control は外側の PTY なし、`--takeover` なしで行い、remote の stable `terminal_id`
  を追跡します。Pane の表示用 `pane_id` は move で変わるため、native terminal handle の
  identity にはしません。

## 共通モデルと識別子

| meeterm | tmux | Herdr |
| --- | --- | --- |
| Connection | SSH host | SSH host |
| Runtime | session `meeterm` | `default` または named session |
| Workspace | window | workspace |
| TerminalGroup | window 内の仮想 group 1 個 | tab |
| Terminal | pane | pane |

tmux の仮想 group は remote object を増やしません。Herdr の tab は TerminalGroup として
保持します。Herdr workspace の cascade/group option は別の remote concept であり、
TerminalGroup に読み替えません。別 group の pane を無条件に混ぜません。group が 1 個なら
group chooser を隠し、複数なら
chooser と pane tabs を表示します。group の作成・改名・選択・削除は native control bridge
から行います。Herdr の TerminalGroup を削除するときは `tab.close` を使います。workspace
削除は `workspace.close` の `close_group: false` とし、workspace と group の cascade を
明示的に分けます。0.9.0 は Herdr 側の終了確認を無効にすると、親 workspace の最後の
pane/tab の終了で、同じ Git repository の関連 workspace まで終了する場合があります。
そのため関連 workspace を持つ親では、meeterm からの pane/group 終了を拒否し、PC の
Herdr で終了対象を確認するよう案内します。操作直前に最新の関連情報を取得し、pane/tab
数の確認後に別 pane が終了する競合も避けるため、この親内の pane/group 終了を一律に
制限します。通常の workspace、関連先のない親、linked worktree 側は操作できます。
workspace 自体の終了は常に `close_group: false` を送り、Herdr の一括終了拒否を維持します。
meeterm は Herdr の終了確認設定や Git worktree のディレクトリを変更・削除しません。
許可された終了操作の結果は snapshot/event で再同期します。

remote ID は SSH、backend、runtime の scope に閉じた opaque 値です。Rust の registry が
`native:<registry>` を安定した terminal ID として native view に渡します。Herdr の外部
`pane_id` が移動で変わっても、stable `terminal_id` に同じ Term と view を結びます。

保存済み profile に backend がない場合は tmux、Herdr runtime が空の場合は `default` です。
既存の SSH credential は SSH/auth/profile の identity として維持し、backend と runtime は
暗号化 credential の AAD identity に含めません。したがって旧 profile の credential を
無効化しません。tmux profile の named runtime は受け付けず、Herdr runtime は upstream の
session 名規則で検証します。

## Native data path

```text
React Native / Expo
  └─ commands, hierarchy, errors, low-frequency metadata snapshots
       ↓
Rust native core
  ├─ backend selector (tmux Control Mode / Herdr direct control)
  ├─ SSH + lifecycle/controller lease
  ├─ stable terminal registry + alacritty_terminal::Term
  └─ Android/iOS native renderer and IME
       ↓ ordinary SSH
remote host
  ├─ tmux session `meeterm`
  └─ existing Herdr 0.9.0 session/socket
```

ANSI bytes、cells、scrollback、render frame、cursor、IME composition は JavaScript の
streaming path に出しません。React Native は navigation、forms、dialogs、group/pane
selection、接続状態、Agent metadata の snapshot を担当し、native view は stable terminal
ID に bind します。Agent status は API metadata を `working`、`blocked`、`done`、`idle`、
`unknown` として保持します。切断・未知状態・offline は成功として表示しません。

## 入力、scroll、resize

表示 frame は入力 mode を表さないため、入力は native 側で意味を保ったまま Herdr 公開
operation に分けます。

- 確定文字列は `pane.send_text`。
- 特殊キーは `pane.send_keys`。Home、End、Insert、Delete、PageUp、PageDown は、0.9.0
  parser が名前を持たないため xterm normal-mode の固定 bytes fallback を使います。
- 貼り付けは UTF-8 と LF をまとめて `pane.send_input` へ送り、必要な complete bracketed
  paste envelope を native 側で明示します。
- remote の表示を入力前に bottom へ戻すため、対象 pane の `pane.scroll` に
  `offset_from_bottom: 0` を送ってから input operation を送ります。
- native view の columns/rows は Herdr の resize operation に伝えます。frame の寸法だけを
  画面へ引き伸ばしません。

controller lease を持つ選択 pane だけが input を送信します。別 controller の競合は明示的な
error とし、release・disconnect・hidden view では新しい input を停止します。通常の PC
client と direct controller が同時に存在できることと、同時編集を順序付ける保証は別です。
Issue の初期 product scope は simultaneous phone/PC editing の保証ではなく、hand-off です。

## lifecycle と PC handoff

画面を hidden にすると `set_terminal_visible(false)` が現在の controller を release し、
SSH と metadata subscription は維持します。release は Herdr stream を closed/EOF まで
drain してから終えます。foreground では stable `terminal_id` を使って再取得し、remote
process を終了させずに snapshot/frame を resync します。background の transport loss は
Rust が bounded reconnect します。

tmux は従来どおり PC から `tmux attach -t meeterm` で同じ window/pane layout を使えます。
Herdr は選択した session を通常の Herdr client から開けます。mobile が phone viewport 用に
保持していた lease/size を graceful release で解放し、ungraceful EOF の後も次の attach が
remote process を再利用できることを handoff の要件にします。PC と phone の完全な同時
操作成功はこの設計の主張ではありません。

## 実装と検証

production Rust code は `Backend::Tmux` と `Backend::Herdr` を共通 `ConnectOptions` から
選び、workspace snapshot、group CRUD、pane selection、visibility、terminal snapshot を
同じ native bridge に公開します。既存の OpenSSH/tmux integration はその backend の検証で
あり、Herdr の公開 API を代用しません。

新しい live integration は次の test です。

```sh
MEETERM_HERDR_INTEGRATION=1 \
MEETERM_HERDR_BINARY=/path/to/herdr-0.9.0 \
cargo test --locked --manifest-path native/meeterm-core/Cargo.toml \
  --test herdr -- --ignored --nocapture
```

`native/meeterm-core/tests/herdr.rs` は test-only の russh SSH endpoint と、隔離 XDG state
で起動する real Herdr 0.9.0 driver を組み合わせます。普通の OpenSSH server fixture では
ありません。default/named runtime、snapshot/subscribe、workspace/group CRUD、ANSI frame、
resize、semantic input、CJK paste、controller conflict、release/reacquire、外部 move と
stable identity を一つの bounded ケースで確認します。公式 binary の CI job は
`RUNNER_TEMP` にだけ pinned digest で取得し、既存環境やユーザーの Herdr session を変更
しません。ローカルのproduction native統合テストは成功しています。通常のPC clientとの
入力・引き継ぎも含む[実測結果と限界](evidence/issue-17-herdr-native.md)を参照してください。
一般CIと両OSの結果は[モバイル受入記録](evidence/issue-17-herdr-mobile.md)で、
対象sourceと検証範囲を分けて記録します。

モバイルでは iOS `standard` の 14 screen に Herdr connection、groups、terminal、workspaces
を含め、Android でも同じ 4 route を fresh process ごとの observational fixture として
撮影します。これらは seeded presentation の確認で、group 作成操作や pixel-diff の gate
ではありません。iOS/Android の画像を実際に review するまで visual success と報告しません。

旧 `scripts/herdr/feasibility.py` の public CLI proof は OpenSSH 経由の先行診断です。新しい
russh integration の代わりにはしません。過去の frame/input の失敗や CI failure は、元の
[evidence files](evidence/issue-17-herdr-feasibility.md) に残したまま扱います。
