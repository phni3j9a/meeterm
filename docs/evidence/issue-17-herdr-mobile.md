# Issue #17: モバイル受入記録

この記録は共有Rust backendの[実Herdr検証](issue-17-herdr-native.md)とは別の範囲です。
画面fixtureの画像は表示状態の証拠であり、実Herdrへ接続してGroupを作る操作の証拠ではありません。

## Production checkpoint e117b06

- Source: `e117b06f09338c299fdcb201ce0f49be18e0e4ff`
- 一般CI: [34676883828](https://github.com/phni3j9a/meeterm/actions/runs/34676883828)、全job成功。
- 両OSのfresh buildとstandard: [34676882950](https://github.com/phni3j9a/meeterm/actions/runs/34676882950)。
- 別のfresh buildによるiOS ssh: [34676906617](https://github.com/phni3j9a/meeterm/actions/runs/34676906617)。standardの再利用productができる前に起動した実行です。

### Android: foundation起動時のANR

build・install・launchの後、`06:08:47.239 UTC`にnative readiness、`06:08:50.643 UTC`に
first-frame markerを記録しました。`06:08:52.613 UTC`にはFocusEventへの応答を5001 ms待った
ANRが発生し、foundationゲートは失敗しました。Herdr 4画面と既存full SSH操作へは進んでいません。

取得した`terminal.png`を実際に開き、空のnative領域と、上部でstatus barに重なるfoundation
見出しを確認しました。frame markerがあっても、この実行の画面表示を成功とは扱いません。
`process.txt`の`mNotResponding=true`とも整合しています。

直前にはBLAST buffer rejectionと37→35→36行へのresizeがあります。ただし、これらの
近接した記録だけでは直接原因を決められません。システムはANR stackを書いたと記録して
いますが、従来のartifactにはstack本体が含まれていません。次のsourceにはapp限定の
ANR trace収集と非rootのDropBox fallbackを追加しました。合格条件・timeout・retryは変更していません。
[Androidの公式ANR診断手順](https://developer.android.com/topic/performance/anrs/diagnose-and-fix-anrs)
に沿って、応答していなかったthreadの証拠を調べます。

これは[先行PoCのAndroid fullでのforeground marker failure](issue-17-herdr-feasibility.md)
とは異なる失敗です。先行結果の名前や成否は変更していません。

### iOS

sshは成功しました（実操作209.42秒、Metal first frame）。ホスト鍵確認、native入力の
リモート到達、切断を確認しています。`ssh-terminal-input.png`と`ssh-disconnected.png`を
実際に開き、端末の出力・キーボード・切断後の案内が読めることを確認しました。
このssh経路は既存tmux接続の検証です。両fresh CNG buildは成功しています。

standardは失敗しました。production保存4件・native入力7件は成功し、14画面中13画面を
撮影しましたが、最後の`herdr-workspaces`のreadinessでSwiftの500行目が失敗し、
その後Xcodeの終了待ちがwrapperの上限に達しました。最終記録は
`stage=xcuitest_standard / reason=xcodebuild_timeout`のまま保持します。
foundationのfresh relaunchには到達していません。

追加したテストは、Workspace行の内側にある「4 ターミナル」「1 ターミナル」を独立した
accessibility要素として探していました。productionの行は明示labelを持つ1個のaccessible
buttonなので、この問い合わせは行の実装と一致していません。修正では既存workspacesと
同じpublic row IDを使い、期待する2行の名前・件数・操作可能な表示を確認します。
各行のPane件数とAgent集計の表示は、撮影後の画像実見で確認します。deadlineは変更しません。

`standard-herdr-connection.png`、`standard-herdr-groups.png`、
`standard-herdr-terminal.png`、既存`standard-workspaces.png`を実際に開きました。
Herdr選択とsession欄、Group chooserとその一覧、選択Group内のTerminal tabs、Agent名と
作業中表示、nativeの日本語/CJK・代表emojiが読めることを確認しました。Herdr一覧の画像は
未取得であり、この実行を4画面全体の表示成功とは扱いません。

## 中間候補の確認範囲

- `39cf3ff` の[実行34678742945](https://github.com/phni3j9a/meeterm/actions/runs/34678742945)は、
  nativeの画面切り替え不具合を修正した次の候補により中止されました。Androidはfoundation
  (`smoke_exit=0`)が成功し、Herdr connection/groups/terminalの3画面まで取得しました。
  foundationと3画面を実際に開き、余白、日本語/CJK、GroupとTerminalの表示を確認しました。
  Herdr一覧と既存full操作は未完了です。この実行では先のANRは発生していませんが、
  e117b06のANRの直接原因が解決したという証拠にはしません。
- `8329ad8` の[一般CI34679263995](https://github.com/phni3j9a/meeterm/actions/runs/34679263995)は成功しました。
  Rust jobで実Herdr統合が16.51秒、実OpenSSH/tmux統合が15.85秒で成功しています。
  [モバイル34679261885](https://github.com/phni3j9a/meeterm/actions/runs/34679261885)は、
  関連Git workspaceへの終了波及を防ぐ修正のため中止されました。
  Androidはfoundation (`smoke_exit=0`)が成功し、この実行にANR/crashはありません。
  Herdr 4画面を取得し、4枚とも実際に開きました。Main workspaceの4ターミナルと
  確認待ち1・作業中1・応答完了1、Tools workspaceの1ターミナルと状態未確認1を確認しました。
  Group chooser、Group内のpane tabs、Agent表示、日本語/CJKが読めています。
  Androidの代表emojiは以前からの単色の輪郭表示で、カラーemojiの再現成功とは扱いません。
  既存fullのpassword formの撮影まで進みましたが、full全体の完了記録はなく、
  jobは新しい候補の実行によって中止されています。
  iOSはproduction保存4件が成功し、standardの名前変更まで9画面を取得、handoff画面を
  開く途中で中止されました。Herdr 4画面や最後のfoundationの合格には読み替えません。
- `2934f8e` と `0d7eab8` の中間実行は、利用手順の整理と共有の名前変更画面の説明修正で
  最終候補へ置き換えました。途中の実行は最終候補の合格証拠に使いません。

## 最終候補

対象は `62a5ce6e50e3bbf83ca0719b09f3a086017e7b76` です。
[PR側一般CI34680787435](https://github.com/phni3j9a/meeterm/actions/runs/34680787435)の
全jobが成功しました。Rust library79件、SSH driver132件、先行Herdr診断6件、Clippyが成功し、
実OpenSSH/tmuxは15.32秒、実Herdrは20.87秒で成功しました。Herdrのケースは
`confirm_close=false` の親pane/group/workspace終了拒否と関連先保持も含みます。
JavaScript/Expo、iOSの事前チェック、Androidのbuild/native unit testsも成功しています。

[Mobile smoke34680785274](https://github.com/phni3j9a/meeterm/actions/runs/34680785274)で
iOS standardとAndroid fullは成功しました。iOS fresh buildは07:29:07–07:45:33 UTCに
成功しました。iOS sshは[再利用実行34681595592](https://github.com/phni3j9a/meeterm/actions/runs/34681595592)で
失敗しました。元のfresh buildと同じcommit/toolchain/hashのpristine test productsを、
専用の再利用経路で復元した実行です。詳細は以下に残します。

### iOS standard: 成功、画像確認済み

production保存4件・native入力7件が成功し、14画面を撮影しました。UI側の
`standard_complete` は287.068秒です。新しいprocessのnative readiness、Metal first frame、
no-crashも成功しています。`ios-foundation-validation.txt` は `result=passed`、
`renderer_backend=metal` です。

`standard-herdr-connection.png`、`standard-herdr-groups.png`、
`standard-herdr-terminal.png`、`standard-herdr-workspaces.png`、foundationの`terminal.png`を
実際に開きました。接続画面のHerdr/session欄、2つのGroupと各2ターミナル、選択Group内の
Code/Shell tabs、Agent名と作業中表示が読めます。Workspace一覧はMainが4ターミナルで
確認待ち1・作業中1・応答完了1、Toolsが1ターミナルで状態未確認1です。日本語/CJK、
結合文字、代表カラーemojiとnative端末が表示され、操作欄と本文の重なりはありません。
これらは明示的なsmoke routeの表示確認であり、実Herdrに接続する操作の実測とは分けます。

### Android full: 成功、画像確認済み

job `103518963006` は39分41秒で成功しました。foundationは `smoke_exit=0` で、
今回のapp ANRはありません。実OpenSSH/tmuxのfullは `result=passed`、
`stage=disconnect_after_resume`、`reason=ok` です。完了markerは69個で、重複はありません。
保存済みprofileと認証情報の復元、background/foreground、process再起動後の再接続、
Workspace/Paneの作成・改名・終了、selection/copy、CJK/ANSI・size、複数paneへの実入力、
PC handoffのlayout、切断・再接続後の入力まで含みます。

Herdr 4画面とfoundationの`terminal.png`を実際に開き、接続欄、Group一覧と選択、
Terminal tabs、Agent名・状態、Main/Tools workspaceの件数を確認しました。日本語/CJKと
native端末は読め、操作欄との重なりはありません。代表emojiは既存の単色表示です。
`ssh-terminal-keyboard.png`と`daily-created-pane.png`も実見し、実SSHの日本語出力、
keyboard/toolbar、改名したWorkspaceとPaneの表示を確認しました。

iOSの`standard-workspace-name.png`も実見し、共有の名前変更画面の説明が
「PC 側にも同じ名前が表示されます。」になっていることを確認しました。

### iOS ssh: marker失敗、入力到達の診断を追加

`34681595592` は `stage=xcuitest_ssh / reason=ui_test_failed` で失敗しました。
ホスト鍵確認とSSH接続は成功し、キーボードの文字入力、Pasteの完了、Returnまで進み、
Swift 570行のmarker待機が失敗しました。これはXcode全体のtimeoutではありません。
最終UI stageは後片付けの `teardown_complete` です。Metal first frameは文字入力開始前に
出ています。入力成功後の撮影には到達せず、画像はありません。

先に成功したe117b06と比べて、この短いSSH testとtmux側のnative入力経路に実装変更は
ありません。今回のHerdr controllerはこのtmux接続テストの入力経路ではありません。
現ログにはremote echoがなく、手入力・貼り付け・Returnのどこで伝達が欠けたかは未確定です。
原因不明のままアプリの入力方法や判定を変えず、Python driverに隔離fixtureの入力到達を
調べる診断を追加します。認証後の該当stageに限り3つのfixture paneを読み取り、期待する
command/貼り付け部分の有無、keyboard prefixの一致文字数、marker結果だけを記録します。
生の端末内容・認証情報・pathは公開せず、入力の再送・assertionの省略・deadline変更も行いません。
テストソース変更のためiOS sshはfresh buildで確認します。アプリとnativeソースは62a5ce6から
変わらないため、上記Android fullとiOS standardの結果・画像確認はそのまま対応します。

### 診断追加後のiOS ssh: 成功、画像確認済み

Pythonの診断と回帰テスト、文書だけを変更した `fe3b7f74879d2b2f3fcaa22423188874a3a5ae01` の
[実行34683723390](https://github.com/phni3j9a/meeterm/actions/runs/34683723390)は成功しました。
この実行でfresh CNG buildを08:37:55–08:51:26 UTC（13分31秒）に行い、同じ実行のpristine
productsをSimulator jobへ渡しました。以前のsourceのbuildは再利用していません。
アプリとnativeは62a5ce6と同一です。

`ios-ssh-validation.txt` は `result=passed / stage=complete`、`ssh_complete` は272.044秒です。
ホスト鍵確認、実SSH接続、キーボードの6文字、native Paste、Return、remote marker、明示切断が
成功しました。Metal first frameも記録されています。追加診断では1つのpaneだけに完全な
command echoとpaste本文があり、keyboard prefixは6文字一致、markerファイルは完全一致でした。
他の2つのpaneにはcommand/markerのechoがありません。生の端末内容を含まない
[到達記録](issue-17-ios-ssh-input-report.json) を保存しています。

`ssh-terminal-input.png`と`ssh-disconnected.png`を実際に開きました。入力後のshell prompt、
native keyboard/toolbar、pane tabsが読め、切断後は未接続状態と再接続案内を表示しています。
秘密欄の画像はありません。この成功は既存tmux経路の実SSH入力確認です。

先の62a5ce6での入力失敗はこの実行では再現しませんでした。診断の追加は本番入力経路を
変えておらず、過去の失敗原因を修正したとは主張しません。失敗時の情報不足とその結果は
上に残しています。assertion、入力方法、再試行回数、deadlineは変更していません。

fe3b7f7の[一般CI34683722369](https://github.com/phni3j9a/meeterm/actions/runs/34683722369)も全job成功です。
新しいiOS driver回帰40件が成功し、実OpenSSH/tmuxは15.53秒、実Herdrは20.71秒で成功しました。
Android fullとiOS standardは、アプリ/nativeが同じ62a5ce6の成功と上記の画像実見を採用します。
実機IME/GPU、HerdrでのモバイルOSによるprocess kill、スマホとPCの同時編集保証、iOSの長い
fullはこの記録の検証範囲外です。Herdrのclient状態喪失はnative owner/registryの再作成で
確認し、AndroidのOS process再起動は既存tmuxのfullで確認しています。
