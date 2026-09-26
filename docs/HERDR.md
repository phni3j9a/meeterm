# Herdr backend

Issue #17 の Herdr backend は、meeterm の Rust/native 経路に実装済みです。
この文書は実装契約と検証範囲を記録します。実 Herdr のnative統合、一般CI、Android full、
iOS standardと短いSSH入力テストが成功し、両OSの画面を実際に開いて確認しました。
対象source、途中の失敗、未検証の範囲は [モバイル受入記録](evidence/issue-17-herdr-mobile.md)
と [native検証記録](evidence/issue-17-herdr-native.md) に残しています。
以前の失敗を含む公開 CLI の実測記録は [feasibility evidence](evidence/issue-17-herdr-feasibility.md)
に保存してあり、書き換えていません。

Herdr は既存の外部アプリです。meeterm は公開 API に適応し、Herdr 本体の変更・fork・
自動導入・更新を行いません。リモートには、SSH から実行できる既存の Herdr 0.9.0 と
SSH stream-local forwarding の許可が必要です。picker には起動中・停止中の対象 session
を表示しますが、meeterm から選べるのは互換性を再確認できた起動中の session だけです。
meeterm 用 gateway、daemon、HTTP/WebSocket relay、追加のリモートツールはありません。

## 接続して使う

1. リモートに Herdr 0.9.0 を用意し、SSH から実行できる状態にします。起動中の
   session はそのまま選択できます。停止中の session も一覧には出ますが、meeterm で
   は選択できません。
2. meetermの接続画面でSSHの接続先・ユーザー・認証方法を入力します。backendや
   session名を固定するのではなく、ホスト鍵確認と認証が成功した後に runtime picker
   の Herdr セクションを表示します。
3. picker で `default` または named session の起動中の row を明示的に選びます。選択時
   に session、protocol 22、schema 1、direct operation、stream-local forwarding を
   再確認します。選択後、ワークスペースを開きます。Herdr の Tab が複数ある場合だけ
   Group の選択が表示され、その中のターミナルを開けます。
4. 作業を残してPCへ戻るときは **切断** を使います。PCでは選択した同じ session を通常の
   `herdr --session default`、または指定した session 名で開きます。

SSHのUnix socket転送が許可されていない場合は、転送設定を確認する案内が出ます。
Herdr未導入、セッション未起動、対応機能・バージョンの不一致、入力権限の競合も
それぞれ別のエラーで案内します。保存済み profile の legacy backend/runtime は
last-used hint として移行しますが、picker を省略しません。既存のSSH credentialと
profile IDは維持し、hintは選択したruntimeが `Ready` になった後だけ更新します。

停止中の row には、通常の Herdr client でその session を開いてから picker を更新する
よう案内します。この issue では meeterm による Herdr の start/create/install/update は
提供しません。停止中の session を選べないことや Herdr のエラーを理由に tmux へ自動で
切り替えることもありません。

### Herdr executable の解決

認証後の discovery では、native core が PATH、公式installerの既定値 `~/.local/bin`、一般的なpackage managerのinstall location
から Herdr 0.9.0 binary を解決します。解決した絶対 path は connection-scoped な native
capability として保持し、runtime list、session status、controller setup、後続の
proof-gated operation のすべてで同じものを使います。path は JavaScript や通常のログへ
渡しません。transport reconnect 後は再解決・再検証してから runtime を再取得します。
非対話 SSH の PATH に `~/.local/bin` が含まれない場合でも、公式installerの既定locationとして
確認できる場所を探索します。解決できない場合は Herdr section の局所エラーとして表示し、
tmux の候補を隠しません。

## 対応する公開プロトコル

現在の互換性ターゲットは **Herdr 0.9.0 / protocol 22 / schema 1** です。
これは下位互換の最低バージョン宣言ではなく、実装と integration test が照合する固定の
公開契約です。公式の [v0.9.0 release](https://github.com/herdrdev/herdr/releases/tag/v0.9.0)
と [v0.9.0 source](https://github.com/herdrdev/herdr/tree/v0.9.0) を参照します。
tag の dereferenced source commit は `b99002ac99b09e00b4ca692436cb15a6b0d676f1` です。

- discovery は `herdr session list --json` と、同じ解決済み binary による各 session の
  status 確認で `default` と named session を列挙します。`listed/stopped` と起動中の
  candidate を区別し、runtime 名は Herdr の session 名として扱います。default は
  `default` です。socket が存在するだけでは互換性確認済みとはしません。
- 解決時に同じ絶対pathの `herdr api schema --json` をboundedに読み、top-levelの
  `protocol == 22` と `schema_version == 1` を必須にします。`status --json` はschema
  versionを公開しないため、status fieldの欠落を互換性の証拠として扱いません。
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
| Runtime | pickerで選択した通常のtmux session | pickerで選択した起動中の `default` または named session |
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

保存済み profile は SSH endpoint/auth の情報を持ち、legacy backend/runtime は
logical な `lastUsedRuntime` hint として扱います。backend がない旧 profile は tmux を既定の候補表示
hint として扱えますが、picker を省略しません。Herdr runtime が空でも `default` へ自動接続しません。
既存の SSH credential は SSH/auth/profile の identity として維持し、backend と runtime は
暗号化 credential の AAD identity に含めません。したがって旧 profile の credential を
無効化しません。tmux は任意の既存 session を picker で選択でき、Herdr runtime は
upstream の session 名規則で検証します。選択した runtime が `Ready` になった後だけ
`lastUsedRuntime` hint を更新します。

## Agent status metadata (Issue #24)

Herdr 0.9.0 の公開 snapshot に含まれる workspace の `agent_status` と tab の
`agent_status` を、Rust の共通 snapshot へそのまま投影します。wire enum から共通 enum
への変換は native の一つの変換境界で行い、workspace/tab の値を pane の走査や
JavaScript の集約で作り直しません。Herdr が返す `unknown` は値のある状態なので
`Some(Unknown)` として保持し、tmux と agent のない pane は `null` です。互換性の根拠は
この文書冒頭で固定している [v0.9.0 release](https://github.com/herdrdev/herdr/releases/tag/v0.9.0)
と [v0.9.0 source](https://github.com/herdrdev/herdr/tree/v0.9.0) です。

共通 bridge の JSON は次の shape です。

```json
{
  "workspaces": [{"id": "…", "name": "…", "agentStatus": "blocked"}],
  "groups": [{"id": "…", "workspaceId": "…", "name": "…", "selected": true, "agentStatus": "working"}],
  "terminals": [{"id": "…", "agent": {"name": "Claude Code", "status": "done"}}]
}
```

`blocked`、`done`、`working`、`idle`、`unknown` が共通の小文字 vocabulary です。Herdr
の workspace/tab rollup は空の階層でも `Some(status)` を維持し、pane は agent 名がある
場合だけ `agent` を持ちます。`pane.agent_status_changed` は既存の subscribe と coherent
full resync を通るため、workspace、tab、pane は同じ native snapshot で更新されます。
`done` を pane 選択時に `idle` へ変更したり、status の優先度で一覧を並べ替えたりしません。

表示側では `Ready` 以外（切断、再接続、runtime 選択中、失敗を含む）の間だけ、保持している
status を灰色の `Status unavailable` として解決します。これは presentation-only の解決で、
native snapshot の値を `unknown` や別 status に書き換えるものではありません。`Ready` に
戻り新しい snapshot を受け取ると、元の status を表示します。値が `null` の場合は mark も
文言も描画しません。

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
  ├─ selected ordinary tmux session
  └─ selected existing Herdr 0.9.0 session/socket
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
競合時は、別の接続で操作権を解放したあとに再接続します。スマホ側の「操作を引き継ぐ」
ボタンや、専用の閲覧モードへ切り替えるUIはありません。
Issue の初期 product scope は simultaneous phone/PC editing の保証ではなく、hand-off です。

## lifecycle と PC handoff

Terminal画面はnative snapshotの選択端末を表示します。PCなどから端末が別のWorkspaceや
Groupへ移動した場合、表示するWorkspace/Groupも同じ更新で移動先へ追従し、同じnative
端末IDを保持します。移動元のWorkspaceがなくなっても、生存する選択端末を表示します。
操作対象の選択はRustが管理し、画面だけ別端末へ切り替えるfallbackは行いません。
選択端末がなくなった場合はnative terminal viewを閉じ、表示状態をnativeへ通知します。
空のGroupは現在のWorkspace内の選択を維持し、Workspace一覧からの自動遷移は行いません。

画面を hidden にすると `set_terminal_visible(false)` が現在の controller を release し、
SSH と metadata subscription は維持します。release は Herdr stream を closed/EOF まで
drain してから終えます。foreground では stable `terminal_id` を使って再取得し、remote
process を終了させずに snapshot/frame を resync します。background の transport loss は
Rust が bounded reconnect します。別の server profile または runtime へ切り替える場合も、
先に現在の controller を release してから新しい actor/binding を取得します。
ただし、明示操作ではない network/channel/transport/remote-close failure の場合は、dead
controller をローカルで abandon し、届かない release ACK を待ちません。明示 Disconnect/Change
と hidden-view handoff は従来どおり release/EOF を drain してから次の acquire に進みます。

Herdr 0.9.0 の公開 API は transport loss の前後で同じ server instance だと比較できる
identity を公開しません。この不足だけでは通常 recovery を止めません。一度 `Ready` に
なった作業は最後の workspace/terminal 画面を read-only で保持し、同じ承認済み SSH host/key
へ再認証した後、compatible な Herdr 0.9.0 / protocol 22 / schema 1 / direct stream-local
contract、同じ選択 runtime、元の stable `terminal_id` の存在を確認します。そのうえで通常の
controller lease を takeover なしで取得し、最初の authoritative full frame が届いた時に
`Ready` と入力許可を戻します。確認画面や Review 操作はありません。

host key 変更、認証失敗、選択 runtime/session または stable terminal の欠落、互換性不一致、
実際の identity mismatch、controller conflict、authoritative frame または必要な再同期の失敗は
具体的な停止条件です。failure 検出時に epoch、入力/操作 gate、transport を直ちに閉じますが、
`stopped` は旧 actor の終了時に公開します。backend が stage した停止理由は維持し、host-key または
authentication failure がある場合はその security reason を優先します。画面内では
`runtimeMismatch`、controller conflict、retry exhaustion/unknown に **Retry** と **Change**、
`runtimeMissing`、`terminalMissing`、`incompatible` に **Change** のみを出します。host key 変更は
**Review key**、認証失敗は **Connection details** を要求します。Workspaces list と Server sheet の
**Reconnect** は reconnecting 中または Retry 可能な stopped reason の時だけ表示し、resynchronizing
中・Change-only・security stop では隠します。
Retry は同じ recovery intent を再試行し、Change は対象を破棄して fresh picker へ進みます。別 runtime
や tmux へ暗黙 fallback しません。保持対象がない cold/fresh connection、明示的な server/Session
change、および target loss 後にユーザーが Change を選んだ時だけ fresh-selection picker を使います。

保持対象がある時の Workspaces list / Server sheet の **Reconnect** と recovery 画面の **Retry** は、
同じ `retryRecovery(id, operationEpoch)` fast path を使います。初回 automatic retry は即時で、
bounded exponential backoff は失敗後だけに適用します。foreground 復帰と network-change 通知は、
foreground かつ automatic reconnect が有効な間、backoff 中の retry を起こします。健康な接続を
切らず、retry budget も reset しません。
stopped owner の Retry は `reconnecting`/`manual_retry` を先に公開してから同じ owner を引き継ぎ、
同じ epoch の重複呼び出しは no-op です。Disconnect/Change は旧 actor 終了後でも recovery intent を
取り消し、遅れて届いた Retry から再開させません。

tmux は選択した通常の session を PC から `tmux attach -t <selected-session>` で開き、同じ
window/pane layout を使えます。Herdr は選択した session を通常の Herdr client から開けます。
`meeterm` は選択した session がその名前の場合にだけ tmux command へ入ります。mobile が
phone viewport 用に保持していた lease/size を graceful release で解放し、ungraceful EOF の
後も次の attach が remote process を再利用できることを handoff の要件にします。PC と phone
の完全な同時操作成功はこの設計の主張ではありません。

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
しません。既に記録したローカルのproduction native統合テスト結果は、その記録にあるsourceの
証拠です。更新後の zero-tap recovery の受入には、今回のcandidateでこのignored testを実行して
成功を確認する必要があります。通常のPC clientとの入力・引き継ぎも含む
[実測結果と限界](evidence/issue-17-herdr-native.md)を参照してください。
一般CIと両OSの結果は[モバイル受入記録](evidence/issue-17-herdr-mobile.md)で、
対象sourceと検証範囲を分けて記録します。

Issue #21 の runtime picker では、discovery の no-side-effect、Herdr PATH 解決、default/named
の running/stopped list、running candidate の再検証、局所的な backend failure、reconnect
identity、profile migration、switch/release を別途確認します。停止中の start/create や
Herdr への自動 fallback は検証対象にも実装 promise にも含めません。linked/shared tmux
topology の安全性は、Herdr の既存 close contract と混同せず、tmux 側の実行直前 fail-closed
検証として記録します。

モバイルでは Android full、iOS `standard`、接続・認証・native input を含む短い iOS `ssh` を
影響範囲に応じて実行します。runtime picker の loading、mixed、empty、partial error、重複名、
明示的作成、stale selection の画面は fixture で確認します。iOS `standard` の source-level
manifest は26画面で、Issue #21の18画面に `session-switcher`、
`session-switcher-sessions`、`recovery-progress`、
`recovery-exhausted`、`recovery-mismatch`、
`connection-error`を加えたものです。
`layout-restore-unconfirmed` と `runtime-layout-restore-unconfirmed` の warning fixture も含みます。
既存の `herdr-connection` は、
Herdr `default` candidate に non-authoritative な `Last used` hint を表示する picker state
です。Android の observational `SCREEN_NAMES` は32 routeで、Issue #21の25
routeに2つの switcher route、3つの recovery route、2つの layout-restore
warning fixtureを加えています。これらは source scope の記述であり、remote CI や visual review の結果を主張
しません。Herdr の実切断からの zero-tap recovery は opt-in ignored Rust/russh integration で
実 Herdr 0.9.0 binary を使って検証します。Android full と iOS `ssh` の実接続切断試験は tmux
を対象とし、iOS standard の Herdr 画面は seeded presentation のみです。seeded presentation は
remote 操作の成功や pixel-diff の gate ではなく、iOS/Android
の画像を実際に review するまで visual success と報告しません。

旧 `scripts/herdr/feasibility.py` の public CLI proof は OpenSSH 経由の先行診断です。新しい
russh integration の代わりにはしません。過去の frame/input の失敗や CI failure は、元の
[evidence files](evidence/issue-17-herdr-feasibility.md) に残したまま扱います。
