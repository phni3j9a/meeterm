# Issue #37: アプリ外観と端末テーマの独立 受入記録

Settings の App appearance と Terminal theme を独立した設定へ分離し、端末 surface の
テーマを既存の native `setTheme` 境界を通して接続・バインディングを保ったまま変更できる
[Issue #37](https://github.com/phni3j9a/meeterm/issues/37) の受入記録です。
検証は source ごとに分けて記録し、到達していない項目は pending とします。

- JS 側の実装は checkpoint `20ebcb9c48166269a5d57d859bbb6881218ecaba` に保存されています
  (Main が JS 所有 path だけを commit)。下の JS source 結果はこの exact source のものです。
  この checkpoint の hosted CI 実行は Rust Unit tests で失敗しました(無変更の
  `network_change_wakes_foreground_backoff…` timeout、232 pass / 1 fail)。通過扱いには
  しません。
- 統合 checkpoint `b8f457758733b6205d80fa4c28c25c3b08af1dab` は、固定 Node 22.22.2 / npm 10.9.7 を使う hosted CI が push・PR の両方で成功しました([push run](https://github.com/phni3j9a/meeterm/actions/runs/36326478627)、[PR run](https://github.com/phni3j9a/meeterm/actions/runs/36326480765))。App テスト130/130、Swift preflight、Rust/OpenSSH/公式 Herdr 0.9.0 integration、Android 生成ビルドと module tests を含みます。追加する iOS 操作ケースは後続 source で再確認します。
- その後の native/test 変更を含む checkpoint `a3695600ab9b6a1223f4d220c9665debfe2f6d03`
  の hosted CI は pass しました: JavaScript/Expo、Rust fmt/unit/OpenSSH/clippy/real Herdr
  integration、iOS fast typecheck(`TerminalInputView` と UI/input XCTest の Swift
  preflight を実際に compile)、generated Android native build + module tests。この host
  には Swift compiler がないため手元では未 compile ですが、初 compile は済んでいます。
  ただし fast typecheck は generated iOS app build・Simulator runtime・Main の
  画像・遷移レビューの受入ではなく、それらは依然 pending です。

## 現在の受入候補と最初のモバイル結果

統合候補 `aa2249ce2f18ad416551ea5a42948d82fb23f6bc` の一般 CI は
[push](https://github.com/phni3j9a/meeterm/actions/runs/36328494218) と
[PR](https://github.com/phni3j9a/meeterm/actions/runs/36328497348) とも成功しました。
追加 XCTest source の Swift preflight、App 130件、Rust/OpenSSH/公式 Herdr、
Android 生成ビルドと module tests を含みます。

iOS `standard` の最初の実行は **失敗**です。fresh CNG/build、storage/input、
26画面、6テーマ組合せと OS 外観切替まで到達し、Metal first frame を報告しましたが、
`theme_dialog_dark` の draft 行確認で止まり、後続の逆ダイアログと foundation relaunch
には到達していません。証跡は
[`5b71dfb` の失敗解析](https://github.com/phni3j9a/meeterm/blob/5b71dfbdb7012f51a48b0eb88fe5c3a18b2a894c/aa2249ce-standard/FAILURE-ANALYSIS.md)
にあります。accessible な設定ボタンの子テキストを XCTest が探していたため、
現在値をボタンの `accessibilityValue.text` で公開し、ボタン自身の値を読む修正を追加しました。
[React Native の値公開 API](https://reactnative.dev/docs/accessibility#accessibilityvalue) を
使用し、別のテスト専用表示や状態は追加しません。15分のテスト予算は超過していません。
手動で draft が Light に変わった画像と、表示中 keyboard の Light/Dark 画像は Main が
実見しましたが、失敗した suite の合格や最終候補の視覚的受入とは扱いません。
Android `full` は同じ候補で機械ゲートと実 SSH・テーマ操作が通りました([`909e58b`](https://github.com/phni3j9a/meeterm/tree/909e58bcf9f71b9ca6cf1e999cd9680820cc5c10))。観察用の画面撮影は31/32で、empty は unavailable と記録されています。これは source manifest32画面や全画面撮影の合格を意味しません。画像の最終レビューと修正後 source の受入結果は別途記録します。

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
- アプリ所有 dialog (AC36): Alert/item action/discard/host-key/close/remove の
  各確認は app 内の `appAlert` seam (`app/dialogs.ts`) を通り、適用済み App
  appearance を引数として渡します。Android は native `presentAppAlert(options)`
  (既存 native module 内の scoped `AlertDialog` 実装) に
  title/message/buttons/cancelable/appearance を委譲し、返却された元 index を
  一度だけ元 callback へ dispatch します。dismissal・範囲外 index・提示失敗は
  callback を呼ばず fail-closed のため、host trust や destructive 操作を
  承認しません。最大3ボタンの Android Alert 意味論と既存ラベル・文言・
  callback 順序を維持します。iOS は既存 RN `Alert`/`ActionSheetIOS` に
  per-dialog `userInterfaceStyle` を渡し、`system` は `unspecified` (sheet は
  省略)で OS 継承します。Activity/window/AppCompat の global appearance や
  `useColorScheme` には触れず、queue/registry/persisted dialog state は
  追加していません。
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
| JS (統合 checkpoint `b8f4577`) | `npm run typecheck` rc=0、`npm run test:app` 130/130 pass。migration seam(未保存→dark)、App/Terminal 4組合せ、両軸 system の OS scheme 追従と固定側の不変、独立 draft/save payload、a11y 行、preview、実 App の TerminalView props/chrome、Settings 開閉の既存1回だけの mount、保存による接続系 native call なし、WorkspaceNavigation の評価済み screen options に加え、アプリ所有 dialog の適用済み appearance 引数(iOS per-dialog style・Android presenter payload)、元 index 一度だけの dispatch、dismissal/範囲外/失敗の fail-closed、draft ではなく適用値を使う picker、実 App 上の host-key/close/remove/discard/group 呼出経路を確認 |
| Rust core | `cargo test --locked` 236 pass・0 fail(新規 theme 3件を含む)、`cargo fmt --check` / `cargo clippy --locked --all-targets -- -D warnings` clean。theme が indexed/truecolor/OSC override を維持し、selection 色が theme に従い、Dark→Light→Dark 変更で Term identity・scrollback・display offset・grid・selection が保持されることを確認 |
| Android JVM | `./gradlew :meeterm-terminal:testDebugUnitTest --offline` BUILD SUCCESSFUL・74 tests pass。legacy 4キー JSON、present-invalid 拒否、dialog の元 button index・一度だけの完了・取消と失敗の拒否・scoped night-mode 選択を含む |
| Python drivers | `python3 -m unittest discover -s scripts/ssh` 253 OK、`scripts/herdr` 6 OK、`scripts/ci` 47 OK (1 skipped)。post-theme SSH marker と Android dialog driver の回帰を含む |
| diff hygiene | `git diff --check` clean |

これらは source 上の unit/harness 検証であり、モバイル受入の証拠ではありません。
ローカルの Node v22.23.2 / npm 10.9.8 は固定版 22.22.2 / 10.9.7 と異なり、
通常コマンドで成功した run と SIGSEGV/SIGTRAP で落ちた run の両方があります。
原因は未確定です。再実行や `--stack-size` を変えた成功を原因解決の証拠とはせず、
統合 checkpoint `b8f4577` は固定版の hosted CI で独立に成功しています。後続の最終候補もその source に対応する CI 結果を記録します。

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
  remote fixture marker round trip を各方向で行い、同じ pane と shell PID から
  Light→Dark の順に応答したことを独立した marker file で検証します。
  light/dark keyboard 撮影も追加済みで、実行結果は pending です。
- Android `full`: 実 Settings 行と navigation から6組合せを操作し、`cmd uimode night`
  による実 OS 切替で process identity を確認、pinned-dark は新規 resolved
  `MEETERM_SMOKE_THEME` marker を出さないことを要求します。App と OS が逆の
  chooser/discard、適用値と draft の区別、App System の OS 継承も確認し、
  `MEETERM_SMOKE_DIALOG` の表示・選択・取消 marker と撮影を残します。
- 画面 manifest は iOS `standard` 26 route・Android `SCREEN_NAMES` 32 route のままで、
  theme 組合せは既存 suite 内の追加 case であり新しい named screen route ではありません。
  撮影物の機械的な pixel 判定は行わず、Main の画像確認まで視覚的成功は保留です。

## 未検証・保留 (pending)

- Swift はこの host(非 macOS・compiler なし)では未 compile ですが、checkpoint
  `a369560` の hosted CI `iOS fast typecheck` で `TerminalInputView` と UI/input
  XCTest の Swift preflight は実 compile 済みです。これは型チェック段であり、
  generated iOS production build・CNG・Simulator runtime の受入ではありません。
- iOS の native dialog の逆 App/OS 組合せと App 固定・Terminal System の実操作を
  `standard` に追加しました。表示中 keyboard の OS 追従と、Discard 後に再 seed せず Settings を開き直す同一プロセスの操作検証も追加済みです。新しい XCTest source の hosted compile・Simulator 実表示は pending です。JS の per-dialog 引数テストを実表示確認の代わりとは扱いません。
- Android の app-owned dialog は scoped native `presentAppAlert` に委譲済みです。
  baseline `a369560` での診断 probe では Main が実際の撮影画像で chooser と
  discard 確認を OS/App 逆組合せの両方で確認し、constraint(現行 dialog が OS
  appearance だけに従う)を pixel 上で実証しました(evidence branch
  `evidence/android-20260927-dialog-probe`)。修正後の generated app での
  theme 組合せ検証・OS 非依存・machine gates の全確認は依然 pending です。
- 最終 exact-source での Android full、iOS standard + ssh suite、証跡 branch・
  capture は未実施です(Main の検証 session と最終 commit 確定後)。
- Main による実画像レビューと画面遷移レビューは未到着です。
- fixture の seed 表示・画面遷移は実 SSH 接続の証拠ではありません。native 入力の
  unit 証拠(marked text 保持)と実 Japanese keyboard の動作、Simulator の
  CG fallback 経路の結果と物理端末の Metal 実経路の parity は、
  それぞれ別の確認として扱います。
