# Issue #37: アプリ外観と端末テーマの独立 受入記録

Settings の App appearance と Terminal theme を独立した設定へ分離し、端末 surface の
テーマを既存の native `setTheme` 境界を通して接続・バインディングを保ったまま変更できる
[Issue #37](https://github.com/phni3j9a/meeterm/issues/37) の受入記録です。
検証は source ごとに分けて記録し、到達していない項目は pending とします。

## 実装範囲

- 設定契約: `TerminalPreferences` に `terminalTheme: 'system' | 'light' | 'dark'` を
  追加しました。既存 `theme` は App appearance のままです。schema 変更・新しい
  storage・新しい依存はなく、JS へ端末バッファや描画データは流していません。
- 既定値: native の新規 App appearance は light です。既存の Web stub
  (`MeetermTerminalModule.web.ts`) の App 既定は system のまま維持しています。
  Terminal theme の既定は dark で、新規設定と `terminalTheme` を持たない legacy
  4キー preferences の両方に適用します (migration seam)。存在する不正値
  (不明文字列・大小文字違い・数値・真偽値・null/`NSNull`) は両 OS の ClientStore で
  既存の storage-error 方針どおり拒否します。
- native 表示: Android/iOS の端末 surface、special-key 行、modifier/copy/paste
  buttons、native selection controls、cursor、IME preedit が解決済み Terminal theme に
  従います。iOS Metal `clearColor` は Light `#FBF7EF` / Dark `#24211D` と一致し、
  iOS 標準 keyboard は `keyboardAppearance` + first responder 中の in-place
  `reloadInputViews()` で Terminal theme に追従します。Android の外部 IME 本体の
  配色は外部アプリ所有のため保証対象外です(ユーザー承認済み)。
- App 側: header、server/session 表示、group/pane tabs、agent status、RecoveryRail、
  通知、操作 UI、navigation 背景 (native-stack の両 screen options を含む)、StatusBar、
  sheets/dialogs は適用済み App appearance に従います。実 Terminal surface と
  unmount 中の placeholder (`terminal-placeholder`) は解決済み Terminal theme に従い、
  `TERMINAL_SURFACE` が native surface と同じ Light `#FBF7EF` / Dark `#24211D` を
  使います。
- アプリ所有 dialog (AC36): Alert/item action/discard/host-key/close/remove の
  各確認は `appAlert` seam (`app/dialogs.ts`) を通り、適用済み App appearance を
  引数として渡します。Android は native `presentAppAlert(options)` (既存 module 内の
  scoped `AlertDialog` 実装) に title/message/buttons/cancelable/appearance を委譲し、
  返却された元 index を一度だけ元 callback へ dispatch します。dismissal・範囲外
  index・提示失敗は callback を呼ばない fail-closed で、`cancelable` の既定 false は
  React Native Android の既存契約を保持します。iOS は既存 `Alert`/`ActionSheetIOS` に
  per-dialog `userInterfaceStyle` を渡し、`system` は `unspecified` (sheet は省略)で
  OS を継承します。未保存の draft は dialog の外観に影響しません。Activity/window/
  AppCompat の global appearance や `useColorScheme` には触れず、queue/registry/
  persisted dialog state は追加していません。
- Terminal 境界: theme 更新は既存 native `theme` prop → `setTheme` →
  `MeetermCore.setTheme` の in-place 更新で、view key・mount 条件・SSH 接続・
  registry・scrollback・選択・retained/recovery 状態・native Term/view handle/
  owner/history/input 状態を保持します。
- Settings UI: 「Appearance」(`app-theme`) と「Terminal theme」(`terminal-theme`) を
  別 draft・別 label/testID の独立行として追加し、各行は現在の draft 値を
  `accessibilityValue.text` で公開します (native UI test が行の選択値を読むため)。
  preview (`terminal-preview`) は編集中の Terminal theme と OS scheme だけで解決します。
  Settings 開閉で composition/selection が解除される既存境界は承認済みの保持対象外の
  ままです。
- smoke fixture: `meeterm://smoke?screen=<名>&app=<system|light|dark>&terminal=<system|light|dark>`
  を smoke flag と明示 route の内側だけに追加しました。`screen` 直後の固定順・限定値のみ
  受け付け、不明値・未知パラメータ・順序入替は従来どおり拒否します。上書きは seed の
  外観だけで、接続・pane・lifecycle 状態は不変です。パラメータなし既定は従来どおり
  light app + dark terminal です。

## 検証対象 source

検証対象の product/test source は C8 `dca158c0239787e1e0066adbe9e9ff83af14cfc8`
です。この文書は実際の product/test 証跡と範囲限定を記録するもので、将来の
ワークフロー完了を記録するものではありません — 現在のレビュー・統合状態は
[PR #44](https://github.com/phni3j9a/meeterm/pull/44) を参照してください。
C5 `de8a4e6` 以降の
C6 `0b12dc6` → C7 `d96cdd7` → C8 `dca158c` は、補正 clip で確定した AC29 視覚欠陥
(FP-003、Android Light native surface の初回 mount/再表示の黒矩形) への bounded
修正チェーンです。C4 `52978906` から C8 までの全差分は `git diff --name-only
52978906..dca158c` の実測で正確に次の **6 path・すべて Android 専用**です
(Main/Sol/JS の独立した実測で確認):

- `modules/meeterm-terminal/android/src/main/java/dev/meeterm/terminal/MeetermTerminalView.kt`
- `modules/meeterm-terminal/android/src/main/java/dev/meeterm/terminal/SurfaceCoverGate.kt` (C6 で新規)
- `modules/meeterm-terminal/android/src/main/java/dev/meeterm/terminal/TerminalRenderer.kt`
- `modules/meeterm-terminal/android/src/test/java/dev/meeterm/terminal/SurfaceCoverGateTest.kt` (C6 で新規、C8 で改訂)
- `scripts/ssh/android_smoke_impl.py` (Android 専用 SSH driver)
- `scripts/ssh/test_android_smoke.py` (上記 driver の test)

共有 App/JS/Rust/iOS native (Swift)/lock/build/CNG/共有 fixture/iOS driver input は
不変です — 変更した driver/test は Android 専用の SSH driver 対のみで、他の
driver 一般が不変という意味ではありません。
source 同一性の主張は **iOS の C4 実走だけ**に適用します — iOS の実 source は C4
`52978906` のまま (同一 C4 products の厳格再利用) で、以下の iOS モバイル記録は
この Android 専用 diff の同一性により C8 へ適用できますが、iOS が C5..C8 のいずれかを
実行した記録ではなく、cross-SHA の product 再利用もありません。
Android 側の C5 `de8a4e6` `full` r3 PASS・transitions/補正 clip は**過去 source の
実結果**として保持します。C8 での fresh `full` r4 は実走済みで 116 stage PASS、
focused probe と合わせて FP-003 は実見範囲で解消しました (下の Android モバイル
検証節)。この証跡のレビュー・統合状態は
[PR #44](https://github.com/phni3j9a/meeterm/pull/44) を参照し、merge 安全性の
主張はここではしません。

## 一般 CI (現在 source C8 `dca158c`)

| source | run | 結果 |
| --- | --- | --- |
| C8 `dca158c` | [push run `36365755778`](https://github.com/phni3j9a/meeterm/actions/runs/36365755778) | **全4 job 成功** |
| C8 `dca158c` | [PR run `36365759677`](https://github.com/phni3j9a/meeterm/actions/runs/36365759677) | **全4 job 成功**。synthetic merge `62a925b7` ("Merge dca158c… into f78fda4…") の tree `14d8edce4826b9c418da572bd4ffa0d407478656` は head `dca158c` の tree と完全一致 (byte-identical) |
| C7 `d96cdd7` | push `36364993698` / PR `36364998753` | 両 run 全4 job 成功 (merge `499d17ab` tree `9ee1ea95…` = head tree)。interim source の通過記録で、最終 source の受入とは別です |
| C6 `0b12dc6` | push `36363548817` / PR `36363552864` | 両 run 全4 job 成功 (merge `c820d2d2` tree `e59fb4bf…` = head tree)。同上 |
| C5 `de8a4e6` | [PR run `36335844322`](https://github.com/phni3j9a/meeterm/actions/runs/36335844322) | 全4 job 成功 (過去 source の記録) |
| C5 `de8a4e6` | [push run `36335842307`](https://github.com/phni3j9a/meeterm/actions/runs/36335842307) | Rust checks の実 OpenSSH PTY で失敗: `tests/openssh.rs:3571` "timed out waiting for TUI transport loss: state=Ready" (50ms poll・30秒 deadline)。Clippy と Herdr integration はこの run だけ未実行、他3 job は成功 |

C8 の job 内容 (両 run 同一 scope): 固定 Node 22.22.2 / npm 10.9.7 で App 130/130、
typecheck・各 check、Swift preflight、Rust unit 236 pass + 実 OpenSSH PTY + Clippy +
公式 Herdr 0.9.0 ignored integration (隔離 russh endpoint、pinned binary は SHA-256
`4fa1a0…f71f` 検証済み)、Android fresh CNG/`assembleDebug` (399 actionable tasks) +
`:meeterm-terminal:testDebugUnitTest` 実行 (C8 の `SurfaceCoverGateTest` を含む)。
Python driver 回帰は `scripts/ssh` 260件 (`test_android_smoke.py` の135件を含む) +
`scripts/herdr` 6件で、Android JVM module test とは別の集合です。

C5 の push 失敗は**原因不明の residual として残します** — PR run は同一 tree
(synthetic merge `06d4bac`、tree `0943142b…` = `de8a4e6` head tree) で全 job pass
しており、50ms の snapshot poll が最後に `Ready` を返した事実は、transient な
`Reconnecting` が存在しなかったことも `Failed` が出なかったことも証明しません
(中間状態を観測しない latest-value のサンプリングのため)。C6/C7/C8 が両 run とも
全 green でも、この residual の flake 証明や原因解明にはなりません — C6..C8 の
差分は Android 専用で OpenSSH/Rust transport 経路に触れていません。
再実行・assertion 変更・deadline 延長は行っていません。
この残余は unmerged delivery のリスク記録であり、merge 安全性の証明ではありません。

## iOS モバイル検証 (実 source `52978906`)

いずれも session `devin-7a32a4e6…` (devin-swe-2-max)、macOS 26.5.2 arm64、
toolchain は実測 node 22.22.2 / npm 10.9.7 / rust 1.96.0 / Xcode 26.6 (17F113) /
iPhone 18 Pro Max Simulator iOS 27.0 です。

**`standard` r3** — evidence [`ios-20260928-issue37-standard-r3` @ `3f15e9be`](https://github.com/phni3j9a/meeterm/tree/3f15e9be75255aeb639f53698d92629034a17aea):
fresh Swift preflight → npm ci → clean CNG → unsigned Release build-for-testing →
install/launch まで全 stage pass。storage 6件 (`legacy_preferences_migration` と旧4キー
補完を含む) と native 入力14件 (`theme_refresh`・`scroll_gesture` を含む) はすべて
passed。26/26 route、6/6 theme 組合せ撮影、Settings preview、反対組合せの seeded
recovery rail、実 `simctl ui` 両方向の外観切替で同一 native handle・selection 維持、
表示中 keyboard の Terminal theme 追従、逆 App/OS の chooser/discard・System dialog、
再 seed なしの discard→再開操作を確認し、`theme_verification_complete` → foundation →
teardown までの 131 stage が完走しました。XCTest は aggregate 15件 (UI 1 +
native 入力14) で 0 fail・323.300秒 (storage 6件は別 artifact)、全体経過
389.7秒 (runner 診断の記録では timeout 上限 890.185秒、従来の15分 XCTest 方針内。
`xcodebuild_forced_exit_after_result=1` は結果行後60秒の documented harness で
deadline failure ではありません)。Metal first frame、SOFTWARE marker 0、no crash。
成果物は PNG 50枚 (26 route・6 組合せ・OS/pinned/keyboard・preview・recovery・
dialog 11状態) と `theme-transitions.mp4` (146.0秒)。
新規起動の fresh build で、products の再利用はしていません。

**`ssh`** — evidence [`ios-20260928-issue37-ssh` @ `ce285376`](https://github.com/phni3j9a/meeterm/tree/ce285376637ec74f4a357a0fc9800bae34798894):
同じ `52978906` の pristine test products を厳格に再利用しました — manifest の
source SHA・Xcode 26.6・arm64・Release-iphonesimulator・sha256 `5caf8f45…de784` を
照合し、新しい `runner-temp-ios37-r3-ssh` に復元しています。新しい fresh build でも
cross-SHA 再利用でもなく、元の fresh build 証跡は r3 `3f15e9b` を参照します。
実 OpenSSH fixture + 実 tmux で、テーマ Light/Dark の marker が各一度・同じ pane・
同じ shell PID から応答しました (other panes clean)。実 transport-loss の前後で
marker 各一度、same pane/PID、native handle・selected pane 同一、cached read-only
surface、loss 中の入力なし、healthy foreground・switcher・disconnect・teardown 完走。
Metal 8 frame・SOFTWARE marker 0。全体 478秒 / 900秒枠、XCTest 413.2秒・0 fail。
PNG は artifact tree 実測で8ファイルです (ssh-entry-initial、ssh-terminal-input、
light/dark 各 surface + 各 keyboard、switcher password-form、disconnected)。
suite の run-record には "7 pngs" と記載されており、実測8との差異はそのまま記録
します (数え方の理由は推定しません)。
ssh suite に mp4 はありません。

## Android モバイル検証 (C5 `de8a4e6` の実走 + C6/C8 focused probe)

session `devin-9429c00e…`、emulator-5554 API 36 x86_64 headless (anims=0.0)。
toolchain 実測: node 22.22.2 / npm 10.9.7 / JDK 17.0.19 / NDK 27.1.12297006 /
Rust 1.96.0 / Gradle 9.3.1。

**`full` r4** (現行 source C8 `dca158c` の最終実走) — evidence
[`android-20260928-issue37-full-r4` @ `cf2db126`](https://github.com/phni3j9a/meeterm/tree/cf2db1264fa6ad17f71312c30c80dc3b1f76fd25):
fresh clean CNG → `assembleRelease` BUILD SUCCESSFUL (2m20s・668 tasks) → install で
`suite=android-full` が `result=passed`・`first_failing_stage=none`、
`device_ready` から `disconnect_after_resume` まで **116 stage 完走**しました。
実 OpenSSH fixture + 実 tmux: connect/auth/host-key 検証、runtime picker、
transport-loss での retained/cached read-only・loss 中の入力 block・pre/post marker
各一度・`native_handle_same`/`pane_identity_same`/other panes clean、manual
reconnect 後の resume、post-theme 変更の実 ACK assertion がすべて PASS。
foundation は `smoke_exit=0`・`NATIVE_READY`×2・`FIRST_FRAME`×1・alive。
ssh validation log の実測値は `NATIVE_READY`×42・`FIRST_FRAME`×21・
`MEETERM_SMOKE_THEME`×30 です (Native 報告の「42両方」表記は FIRST_FRAME を
過剰計上しており、実測の 21 を採用します)。dialog marker は `appearance=dark
resolved=dark`×13・`appearance=light resolved=light`×13・`system` は
resolved=dark×1/resolved=light×7、selected index 0×10/1×9/2×12・dismissed×3。
seeded `SCREEN_NAMES` 撮影は **31/32** — `empty` が
`unavailable:missing_screen_element_0,screen_element_1` で従来 run と同じ観測ですが
**baseline 帰属は未証明**のままです (既存 gate/baseline の正当性を新たに主張
しません)。成果物は計 77 files・PNG 59枚 (旧 C5 bundle の69枚・補正 supplement
10枚込みとは別集計で、混同しません) と `daily-use.mp4` (実 ffprobe
179.834100秒) です。Main が 24枚の指名 PNG (matrix 6組・OS 2・pinned 2・
Settings・dialog 3・discard 2・selection 2・SSH 2・glyph atlas・terminal・
recovery・Herdr) と全6枚の 1fps contact sheet・180 抽出サンプルを実見 —
収録範囲は Settings draft/save/preview・reopen・matrix の最初4組合せ
(App Dark + Terminal Light まで) で、以後の theme pair/OS/dialog/selection は
動画ではなく別 PNG + 完走した assertion が担保します。実見範囲で App/native の
独立 palette・theme key 行・cursor・selection・CJK/ANSI・実 SSH surface が
一致し、native 初期表示は theme 一致の blank → 本文の抽出サンプルで目立つ
黒はありませんでした (全 frame/atomic/pixel の証明ではありません)。

以下の C5 `full` r3 と transitions/補正 clip は FP-003 修正を含まない過去 source
`de8a4e6` の実結果で、C6/C8 の focused probe は修正の目視証跡です。

**`full` r3** (過去 source C5 `de8a4e6` の記録) — evidence [`android-20260928-issue37-full-r3` @ `2ddee407`](https://github.com/phni3j9a/meeterm/tree/2ddee407d228ca10f2385ee2b41b5dbb4193b073):
fresh CNG → `assembleRelease` BUILD SUCCESSFUL (2m28s・668 tasks) → install で
suite=android-full が `result=passed`・`first_failing_stage=none`、`device_ready` から
`disconnect_after_resume` まで **116 stage 完走**しました。実 OpenSSH fixture + 実 tmux:
connect/auth/host-key 検証、runtime picker 選択、同一 server session switch・
cross-endpoint、実 transport-loss (fixture stop/start、cached read-only surface、
`native_handle_same`、`pane_identity_same`、pre/post marker 各一度、loss 中の入力なし)、
backgrounding → foreground で native binding 維持、manual reconnect →
`tmux_pane_resumed` → `remote_marker_resumed` → `process_alive` → disconnect まで
確認しました。Theme legs: r2 で失敗した `daily_settings_theme` が pass (bare-id
matcher 修正が generated app 上で確認)、実 Settings で **6/6 theme 組合せ** と
`MEETERM_SMOKE_THEME` marker 検証、実 `uimode` の OS light→dark→light で同一 pid・
system 追従、pinned dark terminal は OS 切替で repaint/new marker なしの独立性、
app-scoped `presentAppAlert` dialog は `appearance=dark resolved=dark` ×13 (OS light
配下)、`appearance=light resolved=light` ×13 (OS dark 配下)、`system` は
resolved=light ×7 / resolved=dark ×1 で、selected index 0×10/1×9/2×12・dismissed
×3・全て同一 pid です。native foundation gate: foundation launch の logcat に
`NATIVE_READY` ×2 + `FIRST_FRAME` ×1、`metadata.txt` の `smoke_exit=0`。
seeded `SCREEN_NAMES` 撮影は **31/32** — `empty` が
`unavailable:missing_screen_element_0,screen_element_1` で従来 run と同じ観測ですが
baseline 帰属の証明ではなく、観測として記録します。`daily-use.mp4` は実 ffprobe
179.264078秒 (driver の 180秒 cap) で、収録範囲は Settings の実 navigation・draft・
preview と matrix の App Dark + Terminal Light までです — 以後の theme pair・OS・
dialog・selection stage は動画 window の外で、それらは別の実 PNG と machine marker
assertion が担保します (run-record の video scope 表記より実測範囲は狭い)。

**`transitions` supplement** (同一 APK・seeded route・実 `uimode` flip の診断証跡で、
suite 証拠ではありません): `trans-r3-c1.mp4` (実 ffprobe 111.095244秒、要求の
≤120秒以内) と `trans-r3-c2.mp4` (実 102.946744秒、要求の ≤90秒を **超過** —
deviation として記録し、元 capture が上限を守ったとは主張しません)、順序付き still
10枚、`trans-r3-manifest.txt` が同梱されています。manifest の時間表記 (~95/~65秒)
は実測と異なります。Main が全 10 still と両 clip の 2fps 抽出サンプル全て
(222 frame / 8 sheet・206 frame / 7 sheet) を実見した結果、**両 clip/still とも
App Light + Terminal Dark のみを表示し**、要求した c1 `app=dark`+`terminal=system`
と c2 `app=system` の上書きは実画像では示されていません (c1 の seeded recovery
frame を含む)。c2 で OS dark に伴う App dark も視覚確認できず、cloud の
"frame changed" hash は theme 変更の証明ではなく (clock/cursor 等の差異があり
得る)、manifest の pinned-dark / OS-lag 説明は実画像に支持されない未検証のまま
退けています。native pid 5974/6465/6711 の `MEETERM_SMOKE_THEME` marker もすべて
`dark` のみで、`terminal=system` の light resolve や OS 追従の証拠はありません。
これは **diagnostic capture scope の失敗**で、機械 116-stage PASS とは独立に
記録します。

native の bounded read-only 診断 (report `4a6afb06`) は**確定済み**です:
`resolveTerminalTheme('light')` は無条件に `light` を返し `setTheme` は変更時のみ
marker を発行するため、`terminal=light` を seed した pid 6465 が `dark` marker
のみを記録している以上、seed が `preferences.terminalTheme` へ届かなかった
(prop は常に `dark`) ことが分かります — pinned App dark と OS 観測 lag は
要求された `terminal=light` を説明できません。fixture モードでは
`loadPreferences`/`loadProfiles` が早期 return するため persisted preference の
上書き説は否定され、URL の `&app=`/`&terminal=` 構文自体は App parser で受理
されますが、実際に `am start` が受け取った intent bytes は artifact に残って
いません — harness が `&terminal=` を落とした可能性と未追跡の runtime 上書きは
区別できず、**実際の因果 defect は不明のまま**です (同一 APK の実 Settings/OS
legs が通っているため app 側 override defect の証明でも、OS 追従 defect の否定
でもありません)。

**補正証跡** (evidence [`android-20260928-issue37-transitions-corrected-de8` @ `b2f9c18e`](https://github.com/phni3j9a/meeterm/tree/b2f9c18e4b9c7f73041b139ccd3747c4f755b5ec)、13 files、同一 install 済み C5 APK — VM 再起動・AVD state 継続、rebuild/reinstall なし):
FP-002 remediation の bounded probe + corrective capture が実施され、remote argv を
1つに渡して URI を single-quote する修正済み launch は rc=0 で意図した seed を
届けました。旧形式 (unquoted) は remote shell が `&` で分割して rc=127・
`screen=settings` のみ配送 → 公開 Settings draft 行が既定値 (Appearance Light +
Terminal theme Dark = 旧 clip で観測された wrong 値と一致) を示し、修正形式は
rc=0 で Light/Light を示しました — **quoting 機構はこの再現対比で支持されますが、
旧 capture 時の実 `am start` argv/delivered intent bytes は bundle に存在しない
ため旧 clip の因果経路の証明とはしていません** (新形式の raw argv も artifact に
ありません;seeded 公開行・native THEME・実画像の組合せが独立した裏付けです)。
seed-check は Settings route の公開 draft 行で `app=dark&terminal=system` →
Appearance Dark / Terminal theme System、`app=system&terminal=dark` →
Appearance System / Terminal theme Dark を実測 PASS しています。clipA
(実 ffprobe 25.429633秒、≤75秒 cap) は pid 4213 で App Dark + Light native 初回
mount、実 OS light→dark→light を同一 PID で追従、実 native Back → Workspaces →
`workspace-row-@smoke-main` タップで URL reseed なしに既存 workspace を reopen
(Light native + scrollback 復元)、seeded recovery-progress は**別 PID 4668**
(実 SSH recovery でも nav と同一 process でもありません)。THEME marker は 4213
で dark(init)→light→dark→light→reopen dark+light、4668 で dark(init)→light。
clipB (実 10.638856秒) は App System が OS light→dark→light に追従し pid 4969 は
pinned dark のみ (marker dark のみ、contract どおり)。これで FP-002 の意図配色
evidence gap は解消しました — 旧 wrong-theme clip・実測長・失敗記録は保持します。
補正 bundle の THEME は marker のみで、machine readiness/first-frame gate は別の
full `2ddee407` の証跡です。

**欠陥 FP-003 (AC29)**: 補正 clipA の実見で、**アプリ内の native terminal
矩形全体が一時的に黒く表示**されることが確定しました — App Dark header と
Light accessory 行に挟まれたアプリ内部の領域であり、launcher/splash では
ありません。Main が追加で実見した 10fps focus sheet (20 抽出サンプル・2枚) では、
初回 mount で約4.1..4.9秒の連続9サンプルが黒い native 領域 → 約5.0秒で Light
本文、実 Back→reopen で約17.6..18.2秒の連続7サンプル → 約18.3秒で Light 本文です
(時刻ラベルは ffmpeg seek/sample index の近似で、連続時間の正確な PTS 計測では
ありません)。AC29 が要求する Light 初期背景 `#FBF7EF`・mount/navigation 時の
不一致 flash 回避の positive acceptance を阻害するため、C5 `de8a4e6` の最終受入は
取り下げられました。`full` 116-stage PASS・安定 palette PNG は正しい過去の
実結果として残しますが、この machine gate は当該視覚要件を判定しておらず、
PASS が欠陥を相殺しません。

**FP-003/FP-004 の修正経過** (C6→C7→C8、全て Android module のみ):

- C6 `0b12dc6` は最小限の「解決済み theme 色の native cover を有効 frame まで
  被せる」実装を追加しました (新規 `SurfaceCoverGate`、generation  keyed)。
  同一 source の focused probe — evidence
  [`android-20260928-issue37-surface-fix-probe` @ `9148fa33`](https://github.com/phni3j9a/meeterm/tree/9148fa3317e710ef142af4ce1f716b42630f7767)、
  fresh `assembleRelease` 2m20s/668 tasks install、native readiness/first-frame/
  alive gate pass、clip 実 25.354956秒/10.131844秒 (≤75秒 cap) — は Main が全 9 PNG
  と全 3枚の 2fps contact sheet (51+20 抽出サンプル)・10fps 初回10サンプルを
  実見し、**初回黒 viewport は残存** (約4.1..4.9秒の連続9サンプル → 約5.0秒で
  Light 本文) と判定しました。reopen の抽出サンプルは palette 色の blank →
  本文で目立つ黒はなく、clipB は OS return-light 操作を記録したものの最終
  B03/末尾サンプルが App Dark のままなので、その clip での return-Light 完了は
  主張しません (seeded recovery は別 pid 4418 の明示 fixture)。
- C6 の受入済み source 欠陥 (**FP-004**): `setPreserveEGLContextOnPause(true)` の
  下で holder destroy→recreate は `Renderer.onSurfaceCreated` (EGL context event)
  を再実行しないことがあり、C6 の context-generation keyed arm は永久に解除
  され得ないことが source 上確定しました (実機での再現観測ではなく、source/API
  の不整合として受理)。
- C7 `d96cdd7` は seq ベース gate + surfaceChanged/destroyed arm へ改訂しましたが、
  独立レビュー (Sol d28) で「arm 後の再描画が保証されない」可能性が指摘され、
  C8 で holder lifetime keyed gate へ再設計されました。なお d28 の
  「create/resize で後続描画が保証されない」という推論自体は、その後の AOSP 確認
  (`SurfaceView.surfaceRedrawNeededAsync` が追加の surfaceChanged callback **後**に
  `GLSurfaceView.requestRenderAndNotify` 経由で次の描画を要求 — master と
  Android 14 tag で確認、対象 emulator の正確な framework SHA は未同定) を受けて
  **撤回済み**で、C7 の device 障害としては記録しません。Native c754 が仮説として
  挙げた「arm 後に旧サイズ frame が完了し得る」race も、既存 callback serialization
  の下では未証明であり、事実としては記録しません。
- **C8 `dca158c`** は cover gate を holder surface lifetime に key 付け直し
  (context 世代から独立)、frame 開始時に lifetime を stamp し、有効 frame・現在
  token・surface 存在の3条件でのみ reveal、遅延 context-arm を削除しました。
  同一 Sol (af277) の C8 source 再レビューは **material finding なし**で、
  FP-004 の具体的矛盾は source 上解消済みです。ローカル JVM では
  `SurfaceCoverGateTest` 6/6・module 計 80/80 が通りますが、これらは production
  gate メソッドの想定列と個別挙動を模擬する JVM policy test であり、実 Android
  holder/GLThread の結合順序は実行していないため、描画証拠の代替にはしません。

**C8 focused probe** — evidence
[`android-20260928-issue37-surface-lifetime-probe` @ `bd9cfa43`](https://github.com/phni3j9a/meeterm/tree/bd9cfa43e6581b44132448951ecce38b152c078b)
(実 source `dca158c`、fresh `npm ci`+`prebuild --clean`+`assembleRelease`
2m22s/668 tasks → install、native `NATIVE_READY`+`FIRST_FRAME`+alive gate、
quoted-URI seed check が両方向で Settings 公開行と一致して PASS):
Main が 17ファイルを取得し、**全 11 PNG と全 4枚の 2fps contact sheet
(65+22 抽出サンプル)・10fps 初回10サンプルを実見**しました。clipA
(実 ffprobe 32.520967秒)・clipB (実 11.018644秒) は ≤75秒 cap 内です。
実見結果: C6/C5 で黒かった初回 mount の抽出 window は、今回は解決済み theme に
一致した **Light blank → 約5.0秒で Light 本文**になり、実 Back → Workspaces →
既存 workspace 行の reopen (URL reseed なし) と KEYCODE_HOME → MAIN/LAUNCHER 復帰は
**同一 PID 5496** で保持された表示履歴とともに確認でき、実見した抽出サンプルに
目立つ黒矩形・張り付いた cover はありませんでした。clipB では App System が実 OS
light→dark→light に追従し (chrome も両方向に追従)、pinned Terminal は dark のまま、
最終 B03 が Light App まで正しく戻ることを確認しました (C6 capture の未完の
return-Light 観測と異なります)。seeded recovery-progress は**別 PID 6066** の明示
fixture 提示で、実 SSH recovery でも cold process launch でもありません。
これにより **FP-003 の focused な視覚欠陥は実見範囲で解消**しました。

限界 (この probe が主張しないもの): 元動画の全 frame・正確な PTS・atomic な
transition/swap・pixel 一致の証明ではありません。pid 5496 の marker slice では3回の
GLES context-init と3回の `FIRST_FRAME` が記録され (renderer 内の loggedFirstFrame
flag は reset されないため new renderer と推定)、同一 view の preserved-EGL-context
経路は実機では観測されていません — FP-004 の defect path は gate regression
(`recreateWithoutContextRecreated`) と構造 (cover は `onSurfaceCreated` に依存しない)
で担保します。この focused slice には native handle 値がなく、同一 PID + 表示履歴の
みで直接の handle 同一性証明はしません。Native の center-pixel audit は診断用で、
machine gate でも人間による全 frame 証明でもありません (また報告された約6.5秒/
旧4.25秒の可視本文時刻は近似・未裏付けで、Main の約5.0秒の実見を採用します)。

C8 での fresh `full` r4 は上記のとおり実走済みで 116 stage PASS・Main の実見も
完了しています。これらの証跡に対するレビュー・統合の現在状態は
[PR #44](https://github.com/phni3j9a/meeterm/pull/44) を参照してください。

C2 `909e58b` `full` は早期 source の pass のため最終 source の証拠にしません
(失敗履歴の表を参照)。

## Main による実画像・動画レビュー

実施済みの範囲 (記録: `visual-qa/ios-final-review.json` と
`visual-qa/android-final-review.json`。証跡は evidence branch の artifact と一致):

- `standard` r3 の theme 系 PNG 23枚を実見: 6組合せ pair、OS selection 2枚、
  pinned、keyboard、preview、seeded recovery、dialog 11状態。
  `theme-transitions.mp4` は 1fps contact sheet 5枚・146 frame の抽出サンプル
  (146秒分) をすべて実見し、うち実 OS Light→Dark→Light 区間を 5fps・27 frame の
  抽出サンプル (51秒地点から6秒) で focus 確認しました — この主張はその window
  のみに限定します。追加で依頼済みの 71..81秒区間から 5fps・42 frame の抽出
  サンプルを実見: 固定 App Dark の下で表示中の標準
  keyboard/accessory と native Term が Light→Dark→Light に従い、keyboard は切替中も
  表示を維持していました。OS 切替アニメーションの一部サンプル (およそ73.4/76.4秒付近)
  では新旧の surface 色が同一 frame に混在します — 観測事実として記録し、描画の
  原因や全 frame の atomic 性は推定しません。frame ラベルは近似のサンプル index で、
  正確な PTS の証明ではありません。
- `ssh` の PNG 4枚 (ssh-terminal-light/dark と各 keyboard) を実見 — 固定 App light の
  下で native surface と標準 keyboard が独立に Light/Dark へ従うことを確認しました。
  実 remote ACK と pane/shell identity は別の validation file の証拠であり、画像からは
  推定していません。
- iOS 遷移 supplement (同一 installed C4 `52978906`、evidence branch
  [`ios-20260928-issue37-transitions-529` @ `997b6d65`](https://github.com/phni3j9a/meeterm/tree/997b6d65853917a00bc488d531ae443c3ec86407)) を実見: 全6枚の 2fps contact sheet・174 frame の抽出サンプル (export 動画 87.335780秒) と順序付き
  PNG 4枚すべて。内容は Springboard からの実 cold launch (Dark launch アニメーション→
  App Dark 配下の Light native Term)、実 Back → Workspaces → 既存 Main workspace の
  UI tap で同じ pane を remount (URL reseed なし、Light native Term と scrollback が
  復元され App Dark は保持)、および同一 PID 30671 内での明示的な recovery-progress
  seed 更新です — cloud 側の表現は "cold" でも同一 PID のため、これは seeded な状態
  変更であり cold process launch でも実 SSH recovery でもありません。3回の mount で
  すべて FIRST_FRAME_METAL・CoreGraphics 0・crash なしの範囲で確認しました。
  Settings leg は未実施です。sampled な実 Back/reopen/seeded recovery の frame に
  mismatched な Dark native placeholder は見られませんでしたが、全 frame の保証では
  ありません。記録上、元の capture は 105.7秒で、依頼した ≤90秒を超えていました。
  idle な Springboard 先頭と静止末尾を trim して 87.3秒の納品となっています —
  両方の時間をそのまま記録し、元の capture が上限を守ったとは主張しません
  (診断用の視覚証跡であり、機械 budget・suite assertion・deadline の変更は
  ありません)。

- Android `full` r3 (evidence `2ddee407`) の primary 18 PNG を実見: 6 matrix
  組合せ、実 OS light/dark・system・pinned の4枚、逆 App/OS の chooser/discard
  5枚、Settings preview、selection/cleared。実見した状態の範囲で視覚成功 —
  App/Terminal の Light/Dark 独立、実 OS 切替、OS inverse でも App dialog が
  App に従うことを確認しました (Android 外部 IME 本体の配色は承認済みの対象外)。
- `daily-use.mp4` は全 6 枚の 1fps contact sheet・179 frame の抽出サンプルを
  すべて実見 (実 ffprobe 179.264078秒)。収録範囲は Settings navigation/draft/preview と
  matrix の App Dark + Terminal Light までで、残りの theme pair・OS・dialog・
  selection stage は動画では確認できません (別 PNG + machine marker の証拠)。
- Android `transitions` supplement (初回分) は全 10 still と両 clip の 2fps
  抽出サンプル全てを実見 (c1: 222 frame・8 sheet・実 111.095244秒 / c2: 206
  frame・7 sheet・実 102.946744秒)。両 clip/still とも App Light + Terminal
  Dark のみで、要求した逆テーマ上書きは実画像に未実証でした (diagnostic
  scope failure)。これら clip からの保証はしません。
- Android 補正 transitions (`b2f9c18e`) は全 9 PNG と両 clip の全 contact
  sheet を実見 (clipA: 2fps・51 抽出サンプル・2 sheet・実 25.429633秒 /
  clipB: 2fps・21 抽出サンプル・1 sheet・実 10.638856秒)。意図した配色は
  実画像で確認できました (App Dark + Light native mount、実 OS 追従、実 Back →
  既存 workspace reopen、別 PID の seeded recovery、App System 追従 + pinned
  dark)。一方で clipA の Light native mount と reopen にアプリ内の黒い terminal
  矩形が見られ、追加の 10fps focus sheet (20 抽出サンプル・2枚: 約4.1..5.0秒の
  連続9サンプル、約17.6..18.3秒の連続7サンプル) で確定しました — FP-003 の
  AC29 欠陥として上節に記録し、これら clip からの no-flash 受入はしません。
  時刻は ffmpeg seek/sample index の近似です。
- C6 focused probe (`9148fa33`、実 source `0b12dc6`) は 15ファイルを取得し、
  全 9 PNG と全 3枚の 2fps contact sheet (51+20 抽出サンプル)・10fps 初回
  10サンプルを実見: **初回の黒 viewport は残存** (約4.1..4.9秒の連続
  9サンプル → 約5.0秒で本文) で FP-003 は未解消と判定。reopen の抽出サンプルは
  palette 色 blank → 本文で目立つ黒はなく (全 frame 証明ではない)、clipB の
  OS return-light は操作の記録のみで最終サンプルが App Dark のままのため
  return-Light 完了は主張しません。Native の「可視本文4.25秒」「light→black→
  light 機構」等の主張は Main の抽出サンプルに裏付けがなく採用していません
  (seeded recovery は別 pid 4418)。
- C8 focused probe (`bd9cfa43`、実 source `dca158c`) は 17ファイルを取得し、
  全 11 PNG と全 4枚の 2fps contact sheet (65+22 抽出サンプル)・10fps 初回
  10サンプルを実見: 初回 mount の同一抽出 window が **theme 一致の Light blank →
  約5.0秒で Light 本文**に変わり、実 Back → 既存行 reopen (URL reseed なし) と
  KEYCODE_HOME → MAIN/LAUNCHER 復帰 (同一 PID 5496・保持された表示履歴) で
  目立つ黒矩形・張り付いた cover は見られませんでした。clipB は App System が
  OS light→dark→light に追従・pinned Terminal dark・最終 B03 Light App まで
  確認。seeded recovery は別 PID 6066 の明示 fixture 提示です。
  **FP-003 の focused 視覚判定はこの実見範囲で解消**しましたが、全 original
  frame・正確な PTS・atomic/swap/pixel の証明ではなく、同一 view の
  preserved-context 経路・native handle 同一性もこの slice では未証明です
  (上の限界記述を参照)。
- Android `full` r4 (`cf2db126`、実 source C8 `dca158c`) は 77ファイルを
  取得し、**指名24枚の PNG** (matrix 6組・OS 2・pinned 2・Settings・dialog 3・
  discard 2・selection 2・SSH 2・glyph atlas・terminal・recovery・Herdr) と
  全6枚の 1fps contact sheet・180 抽出サンプル (実 ffprobe 179.834100秒) を
  実見しました。動画の収録範囲は Settings draft/save/preview・reopen・matrix の
  最初4組合せ (App Dark + Terminal Light まで) で、以後の theme pair/OS/
  dialog/selection は動画ではなく別 PNG + 完走した assertion の証拠です。
  実見範囲で独立 palette・theme key 行・cursor・selection・CJK/ANSI・実 SSH
  surface が一致し、native 初期表示は theme 一致の blank → 本文の抽出サンプルで
  目立つ黒はありませんでした。全 frame/正確な PTS/atomic/pixel の証明では
  ありません。

限界: 実見は観測済み遷移のサンプルであり、全 frame・全 pixel の判定や
「一切 flash しない」という主張ではありません。cold fixture relaunch は
launch/splash の黒白 frame と Springboard を含みます。Android 側は C5 source で
primary PNG と `daily-use.mp4` の実見が視覚成功済み、supplement の意図配色 gap は
補正 clip で解消し、補正 clip で確定した FP-003 は C8 focused probe と `full` r4
(実見範囲) で解消しました。レビュー・統合の状態は
[PR #44](https://github.com/phni3j9a/meeterm/pull/44) を参照してください。

## 失敗と修正の履歴

| checkpoint | 失敗 (最初の stage) | 対応 |
| --- | --- | --- |
| `20ebcb9` (JS 統合) | 一般 CI Rust unit `network_change_wakes_foreground_backoff` timeout (232 pass / 1 fail)、無変更の既存 test | 通過扱いにせず、後続 checkpoint で再確認 |
| C2 `aa2249ce` iOS `standard` r1 (`5b71dfb`) | `theme_dialog_dark` の draft 行確認で停止 — XCTest が grouped Pressable 内の子 staticTexts を読めない | C3 `99855a25` (MAIN-008): 行の `accessibilityValue.text` に draft 値を公開 |
| C2 `aa2249ce` Android `full` (`909e58b`) | 失敗ではなく早期 source の pass。撮影は31/32 (empty は unavailable)、baseline 帰属は未証明、180秒 daily video は theme legs より前 | 最終 source の証拠にはしない |
| C3 `99855a25` iOS `standard` r2 (`052b34c`) | 全 XCTest 合格後に foundation validator が `malformed_marker` — `MEETERM_SMOKE_THEME` が diagnostic whitelist 未登録 | C4 `52978906` (MAIN-009): THEME diagnostic を light\|dark 限定で whitelist 化 + Python 回帰 |
| C3 `99855a25` Android `full` (`35c51b`) | `daily_settings_theme` が `ui_timeout` — 実 dump が bare resource id (`app-theme`) を出すのに matcher が qualified id のみ要求 | C5 `de8a4e6` (MAIN-010): この package の prefix のみ剥がす normalize + 実 dump XML の回帰 test。135件の Python driver suite を含む `scripts/ssh` 260件が両 CI run で pass |
| C4 `52978906` 一般 CI | push `36334188741` pass / PR `36334191384` が実 OpenSSH "abrupt transport loss" wait で失敗 (byte-identical tree) | 原因不明。C5 とは別 leg の wait。C5..C8 はこの OpenSSH/Rust 経路に触れていない |
| C5 `de8a4e6` 一般 CI | push `36335842307` が実 OpenSSH "TUI transport loss" wait で失敗 / PR `36335844322` 全 job pass (同上の同一 tree) | 原因不明の residual。上の一般 CI 節を参照 |
| C5 `de8a4e6` Android 補正 clip | FP-002: 初回 supplement が意図した seed を届けず AppLight+TermDark のみ — delivered intent bytes 不在のため旧因果経路は未証明 | 補正 bundle `b2f9c18e` で quoting 修正 + seed-check 行実測 PASS、意図配色を確認。旧 clip/失敗記録は保持 |
| C5 `de8a4e6` Android 補正 clip | FP-003: 補正 clipA で Light native mount/reopen のアプリ内 terminal 矩形が一時的に全面黒 (10fps 連続 9/7 抽出サンプル) — AC29 の positive acceptance を阻害 | C8 `dca158c` の native cover 修正後、focused probe `bd9cfa43` と `full` r4 `cf2db126` の抽出サンプルでは消失。黒の root cause は立証せず |
| C6 `0b12dc6` Android focused probe `9148fa33` | 初回 mount の黒 viewport が残存 (約4.1..4.9秒・連続9サンプル)。reopen は抽出サンプルでは palette blank→本文 | 実観測は FP-003 残存を示すのみで、C6 映像が preserved-context 失敗の原因を証明したものではない。C8 probe では限定的な視覚改善を確認 |
| C6 `0b12dc6` source | FP-004: `setPreserveEGLContextOnPause(true)` 下の holder destroy→recreate では EGL context 保持時に `onSurfaceCreated` が再実行されないことがあり、generation keyed arm が残り得る (独立した source/API の不整合で、実機再現観測ではない) | C8 で cover gate を holder surface lifetime に key 付け直し + frame-start stamp + 遅延 context-arm 削除。Sol af277 再レビューで material finding なし |
| C7 `d96cdd7` source | seq gate の設計経過: 「arm 後の再描画が保証されない」推論と「arm 後に旧サイズ frame が完了し得る」仮説が挙がった | 前者は AOSP の `surfaceRedrawNeededAsync`→`requestRenderAndNotify` 確認により撤回済み、後者は既存 callback serialization の下で未証明。事実としての C7 device 障害は記録しない。C8 で holder-lifetime gate へ収束 |

ローカルの Node 22.23.2 / npm 10.9.8 では `npm run test:app`/`tsc` の断続的な
SIGSEGV/SIGTRAP が観測されました (baseline でも再現、原因未確定)。一次ゲートは固定版
Node 22.22.2 の hosted CI で、App 130件の独立 pass があります。

## 検証の限界・別扱い

- fixture の seed 表示と Herdr/recovery route の表示は presentation evidence で、
  実接続・実 Herdr session の証拠ではありません。実 SSH/tmux の mobile 証拠は上記の
  iOS `ssh` (実 C4 source) と Android `full` (現行 C8 `dca158c` の r4。過去 source
  C5 `de8a4e6` の r3 は履歴として保持) の両経路です。公式 Herdr 0.9.0 の
  integration は一般 CI の ignored test (隔離 russh endpoint) が別途確認して
  います。
- Simulator の Metal marker は物理 GPU・font fallback・rotation・日本語 IME の同等性を
  意味しません。marked text の unit 証拠と実 Japanese keyboard の動作は別の確認です。
- Settings を開くと既存どおり composition/selection が解除されます (承認済み例外)。
  Android 外部 IME 本体の配色は対象外 (承認済み)。
- ローカルの source/Python/JS テストは mobile 受入の証拠ではありません。
