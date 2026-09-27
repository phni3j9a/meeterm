# Issue #37: アプリ外観と端末テーマの独立 受入記録

Settings の App appearance と Terminal theme を独立した設定へ分離し、端末 surface の
テーマを既存の native `setTheme` 境界を通して接続・バインディングを保ったまま変更できる
[Issue #37](https://github.com/phni3j9a/meeterm/issues/37) の受入記録です。
検証は source ごとに分けて記録し、到達していない項目は pending とします。

- JS 側の実装は checkpoint `20ebcb9c48166269a5d57d859bbb6881218ecaba` に保存されています
  (Main が JS 所有 path だけを commit)。下の JS source 結果はこの exact source のものです。
- Android/iOS/Rust native 側は peer Worker report 到着済みで、実装と host 上の
  source 検証は完了しています。ただし Swift はこの host で未 compile(初 compile は
  cloud `standard`)、Android Emulator / iOS Simulator の受入と Main の
  画像・遷移レビューは未実施です。review fix(getter-only UIKit API と
  fresh-file legacy test fixture)が進行中のため、下の件数は最終確定ではありません。

## 実装した境界

- 設定契約: `TerminalPreferences` に `terminalTheme: 'system' | 'light' | 'dark'` を追加しました。
  既存 `theme` は App appearance のままで、保存値と新規既定 (light) を変えていません。
  `terminalTheme` を持たない保存データは解決時に dark へ補完され (migration seam)、
  新規設定の Terminal 既定も dark です。両 OS ClientStore でも同じ契約で、
  存在する不正値(不明文字列・大小文字違い・数値・真偽値・null/`NSNull`)は既存の
  storage-error 方針で拒否され、欠落だけが dark へ補完されます。
- native 表示: Android/iOS の端末 surface、special-key 行、modifier/copy/paste
  buttons、native selection controls、cursor、IME preedit が解決済み Terminal theme に
  従います。iOS Metal `clearColor` は Light `#FBF7EF` / Dark `#24211D` と一致し、
  iOS 標準 keyboard appearance は `keyboardAppearance` + first responder 中の
  in-place `reloadInputViews()` で追従します。追加済みの XCTest `theme_refresh`
  (macOS 実行待ち)が responder/marked text/selection/callback の維持を確認する
  設計です。Android の外部 IME 本体の
  配色は外部アプリ所有のため保証対象外です(ユーザー承認済み)。
- Settings UI: 「Appearance」(`app-theme`) と「Terminal theme」(`terminal-theme`) を
  別 draft・別 accessibility label/testID の独立行として追加しました。既存の
  ActionSheet/Alert picker と System/Light/Dark の選択を再利用し、保存 payload に
  `terminalTheme` を含めます。preview (`terminal-preview`) は編集中の Terminal theme と
  OS scheme だけで解決し、App 側の選択には引きずられません。
- 配色の責務: header、server/session 表示、group/pane tabs、agent status、RecoveryRail、
  通知、操作 UI、navigation 背景 (native-stack の両 screen options を含む)、StatusBar、
  sheets/dialogs は App palette。実 Terminal surface とその unmount 中の placeholder
  (`terminal-placeholder`) は解決済み Terminal theme に従い、`TERMINAL_SURFACE` が
  native surface と同じ Light `#FBF7EF` / Dark `#24211D` を使います。
- Terminal 境界: theme 更新は既存 native `theme` prop → `setTheme` →
  `MeetermNative`/`MeetermCore.setTheme` による in-place 更新で、view の key・
  mount 条件・SSH 接続・registry・scrollback・選択・retained/recovery 状態を
  変えません。JS へ端末バッファや描画データは流していません。この
  theme-only in-place 契約は Issue の必須要件であり、JS source テスト(mount 数・
  prop 更新・接続系 native call なし)と Rust unit(変更前後で Term identity・
  scrollback・selection・grid 内容保持)で確認済みです。Emulator/Simulator 上の
  実行時確認は iOS の実 `simctl ui` 外観切替・live SSH theme case と
  Android の実 OS night-mode 切替として実装済みで、remote 実行は pending です。
- 既存ライフサイクル境界: Settings/sheet/modal/background で surface を unmount する
  既存動作を維持します。Settings 開閉で composition/selection が解除される既存境界は
  承認済みの保持対象外です。
- smoke fixture: `meeterm://smoke?screen=<名>&app=<system|light|dark>&terminal=<system|light|dark>`
  を smoke flag と明示 route の内側だけに追加しました。`screen` 直後の固定順・限定値のみ受け付け、
  不明値・未知パラメータ・順序入替は従来どおり拒否します。上書きされるのは seed された
  preferences の外観だけで、接続・pane・lifecycle 状態は不変です。パラメータなしの既定は
  従来どおり light app + dark terminal、foundation preview も決定的な dark のままです。

## source 検証結果 (Worker ローカル実測)

| 範囲 | 結果 |
| --- | --- |
| JS (exact source `20ebcb9`) | `npm run typecheck` rc=0、`npm run test:app` 114/114 pass。migration seam(未保存→dark)、App/Terminal 4組合せ、両軸 system の OS scheme 追従と固定側の不変、独立 draft/save payload、a11y 行、preview、実 App の TerminalView props/chrome、Settings 開閉の既存1回だけの mount、保存による接続系 native call なし、WorkspaceNavigation の評価済み screen options |
| Rust core | `cargo test --locked` 236 pass・0 fail(新規 theme 3件を含む)、`cargo fmt --check` / `cargo clippy --locked --all-targets -- -D warnings` clean。theme が indexed/truecolor/OSC override を維持し、selection 色が theme に従い、Dark→Light→Dark 変更で Term identity・scrollback・display offset・grid・selection が保持されることを確認 |
| Android JVM | `./gradlew :meeterm-terminal:testDebugUnitTest`(pinned NDK install 済み) BUILD SUCCESSFUL・68 tests pass。legacy 4キー JSON → dark 補完と present-invalid 拒否を含む |
| Python drivers | `python3 -m unittest discover -s scripts/ssh` 237 OK(Android driver 126 / iOS 81)、`scripts/herdr` 6 OK、`scripts/ci` の関連 Python green |
| diff hygiene | `git diff --check` clean |

これらは source 上の unit/harness 検証であり、モバイル受入の証拠ではありません。
native 側件数は review fix 前の実測値で、最終確定は peer の最終 report に従います。

## suite 契約の変更 (source-level、remote 実行は pending)

- iOS `standard`: storage case が `legacy_preferences_migration` を含む6件、
  native input case が `theme_refresh`・`scroll_gesture` を含む14件になりました。
  theme 確認として、6組合せの smoke-URL pair 撮影(`theme-app-<a>-terminal-<t>.png`)、
  Settings preview、反対 theme pair の seeded recovery 表示、実 `simctl ui` 外観切替で
  同一 native handle・selection 維持を確認します。pinned-dark は両方向の切替で
  同一 handle のままであることを確認し `theme-os-pinned` を撮影します
  (pinned surface の実表示は Main の画像確認が前提)。run は
  `theme_verification_complete` marker を要求し、
  `ios-appearance-validation.txt` は request/result handshake の補助診断で合格条件では
  ありません。`ssh` では切断前に live `ssh_theme_light`/`ssh_theme_dark` の
  in-place 確認(同一 handle・選択 pane 維持)を行います。theme 変更後の
  remote fixture marker round trip と light/dark keyboard 撮影は MAIN-003 として
  承認済みで、native Worker の次の fix を待つ pending source です。
- Android `full`: 実 Settings 行と navigation から6組合せを操作し、`cmd uimode night`
  による実 OS 切替で process identity を確認、pinned-dark は新規 resolved
  `MEETERM_SMOKE_THEME` marker を出さないことを要求します。
- 画面 manifest は iOS `standard` 26 route・Android `SCREEN_NAMES` 32 route のままで、
  theme 組合せは既存 suite 内の追加 case であり新しい named screen route ではありません。
  撮影物の機械的な pixel 判定は行わず、Main の画像確認まで視覚的成功は保留です。

## 未検証・保留 (pending)

- Swift はこの host で未 compile です。初 compile は cloud `standard` 実行時で、
  review fix(getter-only UIKit API・fresh-file legacy test fixture)適用中です。
- MAIN-003(承認済み)の post-theme remote fixture marker round trip と
  light/dark keyboard capture は native Worker の次の fix を待つ pending source で、
  現行の remote input ack は theme 変更前に完了する既存動作です。
- 最終 exact-source での Android full、iOS standard + ssh suite、証跡 branch・
  capture は未実施です(Main の検証 session と最終 commit 確定後)。
- Main による実画像レビューと画面遷移レビューは未到着です。
- fixture の seed 表示・画面遷移は実 SSH 接続の証拠ではありません。native 入力の
  unit 証拠(marked text 保持)と実 Japanese keyboard の動作、Simulator の
  CG fallback 経路の結果と物理端末の Metal 実経路の parity は、
  それぞれ別の確認として扱います。
