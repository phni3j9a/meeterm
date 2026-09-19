# 標準テスト手順

2026-09-11、利用者の承認により **iOSの通常検証を簡略化**しました。
長い全操作の自動テストを必須にする方針から、画面ごとの表示確認と短い動作テストを組み合わせる方針へ変更します。
Androidの既存full、共有Rustの単体・実SSH/tmux統合テストは維持します。Herdr は公開 protocol
parser/unit test と、実 Herdr 0.9.0 を隔離 russh endpoint から接続する ignored integration
test を追加しています。ローカルの[実Herdr native検証](evidence/issue-17-herdr-native.md)は
成功しています。一般CIと両モバイルの結果は別に記録します。

## 通常の合格条件

| 対象 | 必須の確認 | 結果が意味する範囲 |
| --- | --- | --- |
| 共有コード | TypeScript/Expo、Rustの単体・実OpenSSH/tmux統合テスト、Herdr protocol parser、該当ドライバの回帰テスト | 共有ロジックと接続・端末処理 |
| Herdr live | 隔離 russh endpoint + real Herdr 0.9.0 の ignored integration test | Herdr direct control、snapshot/events、入力・resize・lease・再同期・PC引き継ぎ |
| Android | full smoke（healthy foreground と fixture sshd の deterministic transport-loss → retained/read-only → same-pane Ready → post-loss marker）と画像の実見。source-levelのobservational `SCREEN_NAMES` は29 route（従来25 route＋recovery 4 route） | Androidの自動操作とnative境界。transport-lossの実証はremote emulator実行に限り、fixtureは表示確認だけの代替ではない |
| iOS `standard` | production保存4件、native入力／復旧bridge 11件＋scroll gesture 1件、source-level 22画面の撮影、native起動・readiness・first frame・no-crash | iOSの保存/入力実装、画面表示、実native端末描画 |
| iOS `polish` | 追加7状態、検索・native keyboard・sheet・戻る・edge gesture、fresh native foundation | UI変更時の明示的な追加診断。SSH入力・保存の証拠にはしない |
| iOS `polish-navigation` | 上と同じ操作helperを単独実行し、fresh native foundationを確認 | 端末keyboard/navigationだけの独立診断。7状態や旧polish失敗を合格へ置き換えない |
| iOS `ssh` | 接続、ホスト鍵確認、runtime picker/選択、healthy foreground復帰、fixture sshd の deterministic transport-loss → retained/read-only → same-pane Ready → post-loss marker、切断 | iOSの実SSH、runtime選択、native端末入力とtransport-loss接続境界 |

`standard`をiOSの既定suiteにします。`ssh`は接続・認証・入力・native連携に影響する変更と配布前に実行します。
今回の方針導入時は、fresh CNGでAndroid fullとiOS standardを確認し、同一ソースのiOS sshも確認します。
`full`の全操作成功は、この新しい通常検証や日常利用マイルストーンの必須条件ではありません。
`ssh`/`full`/`names` は Simulator を起動する前に、同じ macOS runner と tmux で
共有Rustの既存runtime一覧・明示選択・Control Mode接続を実OpenSSH経由で確認します。
このpreflightはXCTest runnerが起動できない場合も、remote tmux接続とiOS UI操作の
どちらで失敗したかを分けるためのもので、iOS UI/input検証の代わりにはしません。

## Issue #21 runtime picker の確認項目

runtime picker の変更では、画面だけでなく Rust/native の lifecycle と
backend 境界を確認します。少なくとも次を、実装された source と test の
範囲を明記して記録します。

- discovery が bounded・read-only で、list が runtime を作成・開始・attach
  しないこと。tmux の no-server/no-session、複数 session、名前、明示的な
  detached create、exact identity、list-to-select race を確認すること。
- Herdr の PATH、公式installerの既定値 `~/.local/bin`、一般的なpackage manager locationの解決、同じ native binary capability
  の list/status/controller 利用、`default`/named の running/stopped 表示、
  running candidate の protocol 22/schema 1/direct operation 再検証を確認する
  こと。停止中の start/create や Herdr から tmux への自動 fallback は成功条件に
  含めないこと。
- backend ごとの timeout/output/result bounds と partial failure を分離し、片方
  の欠落・不互換・malformed response がもう片方の候補を隠さないこと。重複名は
  backend を含む identity で区別すること。
- legacy profile の backend/runtime を non-authoritative な last-used hint へ
  移行し、profile ID/credential を保持し、選択 runtime が `Ready` になった後だけ
  hint を更新すること。switch/release は一つの actor を drain/release してから
  次を取得し、remote process を終了しないこと。
- automatic reconnect が同じ `(backend, runtime)` へ戻る前に host、binary/capability、
  server epoch/runtime identity、compatibility を再検証すること。Issue #26以後、Ready済み
  runtimeのmissing/replaced/restarted/uncertainは古いwork screenにfail closedで残り、pickerへ
  自動遷移しません。Herdr 0.9.0 は比較可能なpublic server-instance identityがないため、
  同じwork screen内の明示確認を経てからstable terminal/full frameを検証します。linked/shared tmux topology では workspace close
  と final-pane close を実行直前に同じ Rust actor/control queue で確認し、安全を証明
  できなければ fail closed にすること。

Mobile では picker の loading、duplicate-name、stale-selection、非同期refresh、
explicit selection/create の状態遷移を focused app/native test で確認します。
visual fixture では mixed picker、partial-error、empty、explicit-create の4 routeを
確認します。
Android full、iOS `standard`、接続変更を含む短い iOS `ssh` を適用し、両OSの
スクリーンショットを実際にダウンロードして確認するまで visual success と報告
しません。iOS `standard` の source-level manifest は22画面で、Issue #21の18画面に
`recovery-progress`、`recovery-exhausted`、`recovery-mismatch`、
`herdr-recovery-confirm`を加えます。
`herdr-connection` は Herdr `default` candidate の non-authoritative な `Last used` hint
を示す picker state です。Android の observational `SCREEN_NAMES` は29 routeで、Issue #21の
25 routeに同じ4 recovery routeを加えます。これらは source scope であり、remote CI や visual review の
結果ではありません。

## Issue #26 retained-work recovery の確認項目

同一プロセスで一度Readyになった接続の復旧では、次を一つの受入境界として確認します。

ここでいう復旧には、異なる二つの証拠を分けて記録します。healthy foreground は
Home/background → activate の同一プロセス復帰と、復帰後の native input marker です。
これはOS lifecycleと既存接続の復帰を確認しますが、SSH/Control Mode transportを
切断した証拠ではありません。transport-loss recovery は Android `full` と iOS
`ssh` の実fixture経路で、fixture-owned control fileから disposable `sshd` だけを
停止・再開します。Linux fixtureのstop ACKは、対象portを所有する同一UID・同一sshd実体の
processが残っていないことまで確認し、listener終了後に初めて現れたownerはport再利用の
可能性があるためsignalせず失敗扱いにします。Androidはapp/reverse作成前に対象serialの
emulatorが1台だけ`device`状態でshell UID 2000であること、default
`127.0.0.1:5037` ADB serverを使い、別reverseがないことを検証します。このhost server再起動は
Android fullのGitHub-hosted専用runnerで明示opt-inした場合だけ許可し、local/shared runnerや
endpoint overrideでは拒否します。stop ACK後はexact `tcp:<port>` reverseを削除・消失確認し、
host ADB serverを停止します。ADB commandで自動再起動させる前にraw TCP probeで3回連続の
port閉鎖を確認し、途中でlistenerが戻ればraceとしてfail closedにします。その後serverを開始し、
同じserialだけの`wait-for-device`、同じshell UID、空のreverse list、exact reverseの再作成と
`reverse --list`をすべて照合します。別/offline/unauthorized transport、別reverse、UID/serial不一致、
server停止未観測はfail closedで、停止開始後の失敗にはbounded cleanupでserver復帰を試みます。
tmux server/session/shell、同じhost key/endpointを維持したまま、
pre-loss marker、cached/read-only rail、同じTerminalViewのtest-only native handle、
同じpaneのReady、post-loss markerを順に確認します。切断中のinputは送らず、markerは
各一回で別paneに現れないことをfixture側で検証します。iOSの固定artifactには、surface
bindingとは別に実際のstale/recovered handle比較を表す
`native_handle_same=yes`と、stale/recovered各時点のselected pane観測を表す
`selected_pane_identifier_same=yes`を出力します。

- transport loss/foreground復帰から、最後のworkspace、選択terminal、native handle、Term、
  history/scroll/selectionを保持し、pickerを自動表示しないこと。stale画面は必ずread-only表示とし、
  key、IME、paste、terminal自動応答、resize、pane/group/workspaceのremote mutationを拒否します。
- connection generationと別のoperation epochで、loss前に開始した非同期paste/IME/resize/control
  callbackをloss→Ready後も拒否すること。拒否した操作や不明な送信を後からreplayしません。
- tmuxはstored session identityと元pane IDを必須とし、attach後の同じControl Mode streamで
  再検証します。初期同期、選択/zoom後のdirty readback、元paneのauthoritative captureが
  終わるまでReady/inputを公開しません。missing/replaced/stale topologyで別paneへfallbackしません。
- Herdrは確認前のcontroller取得・入力・mutationをゼロにし、one-use tokenの確認後も候補、
  protocol 22/schema 1/direct API、元stable `terminal_id`、通常lease、最初のfull frameを再検証
  します。takeover、別terminal、tmux fallbackは行いません。
- retry枯渇・identity mismatch・terminal missing・controller conflictはcached画面内で停止し、
  RetryとChange connection/runtimeを提示します。明示disconnect/changeは旧recovery/tokenを取消し、
  fresh manual/cold connectだけが従来どおりpickerを通ります。

focused Rust/App/native adapter testsの後、exact candidate commitで実OpenSSH/tmux、公式Herdr
0.9.0 ignored integration、Android full、iOS `standard`、iOS `ssh`を実行します。Android fullと
iOS `ssh`のtransport-loss caseはそれぞれのremote jobで初めて実transport証拠になります。
ローカルのsource/PythonテストだけではCI mobile successを主張しません。4つの
recovery fixture routeはpresentation evidenceであり、実loss、identity確認、入力拒否の証拠には
代用しません。Android/iOS両方の最終screenshotをdownloadして実際に開くまでvisual successを
報告しません。

一般CIのRust jobは公式 Herdr v0.9.0 binary を `RUNNER_TEMP` にだけ取得し、SHA-256
`4fa1a01158dd8043da92d31b270780b0dcc10603038d9b61cac4d81ab63fb71f` を検証してから、
`native/meeterm-core/tests/herdr.rs` の ignored test を実行します。ユーザー環境や Herdr
session を変更しません。ローカル成功とGitHub CIの結果は区別して記録します。

スクリーンショットは実際に開いて確認します。画像の存在やpixel diffを新しい機械ゲートにはしません。
画像だけから保存・接続・コピー・名前変更の成功を主張しません。seedされた画面と実操作の証拠を区別します。
未確認の操作と既知の失敗は [DAILY_USE.md](DAILY_USE.md) に残します。

## 変更に応じた実行範囲

| 変更 | 最初の確認 | Mobile検証 |
| --- | --- | --- |
| 文書のみ | 記述・リンク・実コマンドとの整合 | 再実行不要 |
| Python/成果物処理 | 対象Python回帰 | 影響するsuite |
| iOSの撮影・自動操作 | Swift型チェック、該当回帰 | iOS standard、実接続への変更ならsshも |
| UI・画面fixture | TypeScript、Swiftの該当チェック | Android fullとiOS standard、両OS画像の実見 |
| Rust/native/接続入力 | 該当単体・統合・型チェック | Android full、iOS standardとssh |
| 依存関係・CNG・Mobile workflow | 型・スクリプト・生成設定のチェック | fresh CNGからAndroid fullとiOS standard、接続への影響に応じssh |

長いビルドの前に短いチェックを行います。成功済みで影響のないテストを理由なく繰り返しません。
アプリ/native変更のpushはMobileの通常検証を起動します。テスト・スクリプトのみの変更では、一般CIの後に必要なsuiteを手動指定します。

```sh
npm run typecheck
npm run test:app
python3 -m unittest discover -s scripts/ssh -p 'test_*.py'
python3 -m unittest discover -s scripts/ci -p 'test_*.py'
git diff --check
```

全コマンドを毎回実行する必要はありません。変更に関連するチェックを選びます。
`test:app`は実際の`App.tsx`をReactで動かし、native bridgeの状態取得をテスト用snapshotに
置き換えます。通常の画面操作とsnapshot更新を通して、選択端末の外部移動、移動元Workspaceの
消失、Groupと空のGroup、画面のnative端末IDと表示状態の通知を確認します。React Nativeの
host viewとnative bridgeはテスト用なので、実機の描画やHerdr controller自体の検証とは分けます。
空のGroupの選択はnative側でキューに入るため、選択要求の直後は古いsnapshotが返り、
後続の更新で選択が完了するケースも確認します。
macOSでは `scripts/ci/ios-typecheck.sh` がCNG/build前にUI XCTestとnative入力関連Swiftを型チェックします。
production moduleへ依存する保存テストのコンパイル・Keychain実行はアプリビルドとnativeテストで確認します。

## 画面の撮影方法

通常のアプリでフォーム入力・接続・作成を順に実行してから撮影する方法を、表示確認の前提にしません。
smoke buildと明示したテスト起動URLを組み合わせ、固定の公開データで対象画面を直接開きます。
本番と同じ画面コンポーネントを使い、撮影用の画面を別実装しません。

対象はホーム、保存済みサーバー、鍵認証フォーム、パスワード認証フォーム、
ワークスペース一覧、ターミナル、設定、ワークスペース名、ターミナル名、PC引き継ぎに加え、
`runtime-picker`、`runtime-partial-error`、`runtime-empty`、`runtime-create`、
`herdr-connection`、`herdr-groups`、`herdr-terminal`、`herdr-workspaces`、
`recovery-progress`、`recovery-exhausted`、`recovery-mismatch`、
`herdr-recovery-confirm`を含むiOS `standard` のsource-level 22画面です。
`herdr-connection` は旧backend/session formではなく、
Herdr `default` candidate の `Last used` hint を示すpicker stateです。
`meeterm://smoke?screen=<名前>` で直接開き、`standard-<名前>.png` に保存します。
名前は順に `home`、`servers`、`connection`、`password`、`workspaces`、`terminal`、
`settings`、`workspace-name`、`terminal-name`、`handoff`、`runtime-picker`、
`runtime-partial-error`、`runtime-empty`、`runtime-create`、
`herdr-connection`、`herdr-groups`、`herdr-terminal`、`herdr-workspaces`、
`recovery-progress`、`recovery-exhausted`、`recovery-mismatch`、
`herdr-recovery-confirm`です。
撮影用設定はライト表示に固定します。最後の新規起動によるnative foundationは `terminal.png` に保存します。

追加診断の `polish` は、初回起動、空の一覧、検索結果なし、切断、再接続中、認証エラー、
長いworkspace名の7状態を `polish-<名前>.png` に保存します。起動URL名は
`welcome`、`empty`、`search-empty`、`disconnected`、`reconnecting`、`connection-error`、`long-workspaces` です。
その後に、検索→既存native fixture端末→キーボード開閉→設定→workspace切替sheet→
戻る→iOS端からの戻るジェスチャを実際に操作します。検索条件の保持もassertします。
実際のkeyboard表示とedge back後の検索保持も、それぞれ `polish-terminal-keyboard.png` と
`polish-edge-back.png` に記録します。7状態のseed画像とは別の実操作後の画像です。
この区間だけ既存の録画機構で `daily-interactions.mp4` を記録します。
fixtureは既存 `poc-main` を開くことだけを許し、接続・遠隔操作・端末データの生成は行いません。
これはnavigation/keyboard表示の検証であり、SSH入力の証拠にはしません。
Android の observational `SCREEN_NAMES` は29 routeです。従来25 routeに
`recovery-progress`、`recovery-exhausted`、`recovery-mismatch`、
`herdr-recovery-confirm`を加えたsource-level scopeで、
任意の画像を採取します。既存full gateとdaily-use録画は維持し、source scopeと実際のCI・画像確認は
別々に報告します。既存の900秒枠を延長せず、
検証範囲を分けて同一ソースのpristine test productsを再利用します。

`polish-navigation` は上記の検索・端末keyboard・sheet・戻る操作を同じhelperで単独実行し、
その後にfresh native foundationを確認します。7状態の起動巡回への依存を避けて、未確認の
操作区間を直接調べるための診断です。既存 `polish` の内容と完了条件は変更しません。
独自の完了記録に加え、操作helperとfoundationの完了をすべて要求し、旧suiteの記録や
画像だけで成功とは判定しません。成果物は実行ごとに分離し、実keyboard・edge Back後の
2画像とfoundationを確認します。7状態の表示・実SSH・保存の成功は主張しません。

`standard` / `polish` / `polish-navigation` の公開fixture内で失敗した場合だけ、
`public-presentation-failure.png` と要素の存在・操作可能性・矩形を記録します。
`-meeterm-ui-observation` 起動引数はこの三つと短い `ssh` テストだけが渡し、native入力の
focus/window/bindingとPasteのrequest/provider/drop/delivery/accepted状態を固定形式の
ログに残します。smoke iOS起動時にはJS module、initial URLの固定分類、AppContent、
profile取得とruntime discoveryの到達phaseも、nativeのallowlistを通して記録します。URLそのもの、入力文字、
composition、clipboard、profileやremote IDは記録しません。
短い `ssh` の撮影許可は公開fixtureと分離し、最初の接続情報の入力前だけに限定します。
foreground到達後の `ssh-entry-initial.png`、接続入口で失敗した場合の
`ssh-entry-failure.png` と `ios-ui-ssh-entry-diagnostics.txt` を任意の観測として保存します。
非foregroundでは要素照会や撮影を行わず、取得できない理由を固定値で残します。
実SSHやformsの失敗を無条件に撮影する機能ではありません。
foundation判定では、これらの固定診断をreadiness/frameから分離します。
入力診断だけでは合格にならず、不正な値や未知のmarkerは引き続き失敗になります。
fixtureも実際のAppState通知に追従しますが、Rustへの接続・再接続呼び出しは行いません。

小画面・大きい文字の明示的診断には workflow_dispatch の `ios_profile=compact-xl` を使います。
同一commitのpristine test productsを指定して再利用できます。SE（第3世代）の新規Simulatorを
作成し、OSのcontent sizeをextra-largeに設定して読み戻しを記録します。通常のPro系端末の
結果と区別し、別runの画像として確認します。対応runtimeがなければ失敗を明示し、
大型端末を小型端末と称するfallbackは行いません。

- 保存済みサーバーやworkspace/paneの情報は表示用fixtureです。実サーバーで作成した証拠にはしません。
- Herdr fixture は backend/runtimes、group、Agent metadata、native terminal の表示を確認します。画面上のseed状態は group作成や接続操作の成功を証明しません。
- 撮影準備で秘密鍵を入力したり、実ユーザーの保存情報を書き換えたりしません。
- 端末の表示には既存のRust fixtureとnative TerminalViewを使います。JSで端末データや画像を模造しません。
- 通常起動とsmoke無効のビルドでは撮影用経路を有効にしません。
- 最後のnative foundationは新しい起動のreadiness/frameとforeground維持を確認します。

各画面の到達記録を残します。撮影できなければ理由を残し、Mainの画像確認が済むまで視覚的な成功は報告しません。

## suiteと実行例

| `ios_suite` | 用途 |
| --- | --- |
| `standard` | 通常の保存・入力・画面撮影・native foundation。既定値 |
| `polish` | 追加7状態と検索・native keyboard・sheet・back gestureの表示・操作診断 |
| `polish-navigation` | 同じ操作helperとfresh foundationを、7状態の巡回から独立して確認 |
| `ssh` | 実SSH接続と短いnative入出力の確認 |
| `native` | 保存4件（legacy profileのbackend/runtimeをlast-used hintへ移行する境界を含む）とnative入力7件＋scroll gesture 1件の限定確認 |
| `forms` | 接続フォームの実操作を調べる任意の診断 |
| `names` | 実SSH経由のworkspace/pane作成・名前変更・終了を調べる任意の診断 |
| `full` | 従来の全操作、cold restart、copy、設定、名前操作等を連続実行する任意の診断 |

```sh
test_ref="$(git branch --show-current)"
# 通常の両OS検証（Androidは従来full）
gh workflow run mobile-smoke.yml --ref "$test_ref" -f platform=both -f ios_suite=standard
# iOSだけを調べる場合
gh workflow run mobile-smoke.yml --ref "$test_ref" -f platform=ios -f ios_suite=standard
# 実SSHの確認
gh workflow run mobile-smoke.yml --ref "$test_ref" -f platform=ios -f ios_suite=ssh
```

`standard`と`native`、`forms`はSSH fixtureを起動しません。
`ssh`では実ホスト鍵を確認し、実入力がfixture内へ到達することを要求します。画像上の接続表示だけでは合格にしません。
`full`は必要時に明示して実行し、失敗はそのまま記録します。任意の診断が未通過であることと通常検証の合否を分けます。
既存のcopy observerのタイムアウトを、合格済み・修正済みへ書き換える変更ではありません。

## ビルドと再利用

iOSのSwift事前チェック、fresh CNG build、Simulator runtimeは別ジョブです。
ビルド時間が操作テストの制限時間を消費しない構成を維持します。
`standard`と`ssh`はそれぞれXCTest全体15分、`native`は10分、`forms`/`names`は15分、任意`full`は30分が上限です。
Simulator起動等の時間はこのXCTest実行枠とは別です。実行時間は結果とともに記録し、短縮幅を推測で報告しません。

同一commitの別suiteや原因調査では、ビルド済み成果物を再利用できます。

```sh
# BUILD_RUN_IDを同一ソースのビルド成功runに置き換える
gh workflow run mobile-smoke.yml --ref "$test_ref" \
  -f platform=ios -f ios_suite=ssh -f ios_build_run=BUILD_RUN_ID
# 同じ製品で小画面の端末keyboard/navigationだけを確認する場合
gh workflow run mobile-smoke.yml --ref "$test_ref" \
  -f platform=ios -f ios_suite=polish-navigation -f ios_profile=compact-xl \
  -f ios_build_run=BUILD_RUN_ID
```

GitHub runのcommitとmanifestのcommit・Xcode version/build・CPU・構成・SHA-256を照合します。
Swift/アプリ/テストソースを変えたら新しいビルドが必要です。同一バイナリでsuiteを分けて確認する際の再ビルドを省きます。
受入記録には元のfresh buildと再利用先の両runを記載します。再利用先で新たなCNG/buildを実行したとは記録しません。

`ios-test-products`はfixture環境変数注入前のpristine tar/manifestで、保持7日です。
環境注入は実行ごとの一時コピーだけに行い、raw XCTest/xcresultや秘密情報を成果物へ含めません。

## 失敗時の調べ方

| 成果物 | 内容 |
| --- | --- |
| `ios-build-observability` | CNG/buildログ、toolchain、起動前診断 |
| `ios-simulator-observability` | suite別合否、段階・時刻、公開画面、sanitized nativeログ |
| `ios-test-products` | 同一ソース再利用用のpristine成果物 |

```sh
gh run download RUN_ID --name ios-simulator-observability --dir /tmp/meeterm-evidence-RUN_ID
```

1. 最初の失敗をbuild、Simulator、保存/入力、撮影、実SSH、native描画に分けます。
2. 固定診断、stage、時刻、画像を確認します。画像や動画は実際に開きます。
3. 失敗した最小の処理を先に切り分け、長いfullへ戻ることを既定にしません。
4. 原因に対応した修正と短い検証の後、影響するsuiteを実行します。

選択したsuiteの必須テスト、正常終了、fresh完了記録は維持します。タイムアウトを成功へ変えません。
固定sleep・盲目的なretry・汎用Continueの無条件tapを追加しません。
XCTest開始前の終了も調べられるよう、`xcodebuild`の通常出力をRUNNER_TEMP内だけに保持します。
失敗時はAppleの`xcresulttool get test-results summary`も上限10秒で読み、固定分類・件数と
既知のApple/POSIX error domainの整数コードだけを公開します。失敗文・userInfo・パス・
任意のdomain名やraw summaryはアップロードしません。診断の失敗は元の合否を変えません。
OSの初回案内は固有の文章を確認して一度閉じ、消失後に通常操作を行います。
端末のキー待機失敗では `ios-ui-terminal-keyboard-diagnostics.txt` を確認します。
`simulator-log-collection.txt` はログ取得元、コマンド成否、smoke markerの有無を分けて
記録します。失敗時も `launch.txt` の検証済みUTC開始時刻から取得し、旧成果物などで
開始時刻がなければ `--last 10m` の限定fallbackを明示します。ログ取得成功や診断marker
だけを起動・描画・入力の合格証拠にはしません。Pasteの `Ready` もproviderの状態であり、
入力がremoteへ届いた証明は従来どおりremote acknowledgmentに依存します。
実接続失敗では保存metadataの一致フラグとstrict SSH probeを確認できますが、事後probe成功だけでUI入力成功は証明できません。
短いSSH入力のmarker待機まで進んだ実行では、`ios-ssh-input-diagnostics.json`に隔離fixtureの
command echo、手入力とpasteの到達、markerの一致をbooleanと件数で残します。生の端末内容は
保存しません。この事後診断は入力を再送せず、元のXCTestの成否を変えません。
秘密欄の画像や入力値、rawリモートエラーを診断に残しません。

## 受入記録と限界

suite、commit、run URL、fresh build/再利用元、実行時間、実見した画像と未検証項目を記録します。
MetalとSimulator専用CoreGraphics描画を区別します。実機GPU・日本語IME・フォントの同等性は、実機で確認するまで未検証です。
スクリーンショット中心の通常検証は、iOSの全操作・OS clipboard・全ライフサイクル経路の保証にはしません。

最新の結果は [DAILY_USE.md](DAILY_USE.md)、評価APKは [FIRST_APP.md](FIRST_APP.md)、
旧方針での結果と失敗調査は [検証履歴](evidence/testing-method-validation-history.md) に保存しています。

### 導入時の実測

`b82c226` の [初回実行](https://github.com/phni3j9a/meeterm/actions/runs/34590304287) では、
iOSのfresh CNG/buildは17分13秒、standardの保存テストは65.4秒、入力・画面撮影・foundationは288.9秒でした。
テスト本体は合計約5分54秒で成功し、10画面とfoundationの11枚を実見しました。
初回ビルドとSimulator準備の時間は、この5分54秒に含みません。
従来fullとは確認範囲が異なるため、同じ内容の単純な高速化として比較しません。
同じビルドを再利用した [ssh実行](https://github.com/phni3j9a/meeterm/actions/runs/34591998968) は、
XCTestが522.1秒（約8分42秒）で成功しました。実フォーム入力も含むため、通常の画面確認とは分けて実行します。
