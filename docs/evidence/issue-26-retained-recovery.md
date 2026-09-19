# Issue #26: retained-work recovery 受入記録

この記録は、一時的なSSH/Control Mode切断と同一プロセスのforeground復帰で、
保持済みの作業画面から同じruntime・同じTerminalへ安全に復旧する範囲を扱います。
fresh manual/cold connectionのruntime picker、host-key検証、明示的なruntime選択は維持します。

## 実装した境界

- transport切断だけでは最後に同期済みのWorkspace / Group / Terminal、Rust-owned `Term`、
  native view、履歴・scroll・selectionを破棄しません。stale表示はread-onlyにし、通常の復旧で
  pickerを自動表示しません。
- 接続generationとは別のoperation epochで、key、IME、paste、terminal自動応答、resize、
  pane/group/workspace mutationをfail closedにします。結果が不明な入力を再送・後送りしません。
- tmuxは元session identityとpane IDを、再接続後の同じControl Mode streamで再検証し、
  全対象paneのauthoritative captureと選択確認を終えてからReadyに戻します。
- Herdrは候補、protocol 22、schema 1、direct API、stable `terminal_id`、通常lease、最初のfull frameを
  再検証します。server-instance continuityを自動証明できない復旧では保持画面内の確認を要求し、
  takeover、別Terminal、tmux fallbackは行いません。
- Retryは同じ作業への復帰、Change connection/runtimeとDisconnectは旧epochを取消す明示的な
  新規選択として分離しました。retry枯渇、identity mismatch、Terminal消失、controller競合でも
  cached画面を保持します。
- Android fullとiOS sshの実fixtureは、同じhost key/endpointとtmux server/session/shellを維持して
  disposable `sshd`だけを停止・再開します。Linuxではexact portのsocket ownerをfixture UID、
  resolved sshd executable、PID start identityで限定し、TERM、bounded wait、KILL、再確認を終えるまで
  stop ACKを返しません。listener終了後に初めて現れたownerはport再利用の可能性があるためsignalせず、
  fail closedにします。

## 中間候補で分かったこと

### e8b24e1

[`35436003531`](https://github.com/phni3j9a/meeterm/actions/runs/35436003531)では、
Androidのtransport-loss fixtureがlistener停止だけで成功ACKを返し得ることが分かりました。
iOS standardの`herdr-recovery-confirm`はfixture runtimeが`dev`であるのに旧期待値`meeterm`を
照合して失敗しました。Expo Doctorも同じSDK内の推奨patchとの差を報告しました。
assertionやdeadlineを弱めず、runtime期待値、fixture ownership、Expoの推奨patchだけを修正しました。

同じsourceのiOS ssh
[`35436013256`](https://github.com/phni3j9a/meeterm/actions/runs/35436013256)はfresh CNG buildから成功し、
transport loss、cached/read-only、同じpaneのReady、post-loss markerまで通過しました。

### 724e15f

一般CI[`35438487819`](https://github.com/phni3j9a/meeterm/actions/runs/35438487819)は全job成功しました。
Mobile smoke[`35438484693`](https://github.com/phni3j9a/meeterm/actions/runs/35438484693)では、
Androidが`stale_read_only_timeout`で失敗しました。stop ACK、`adb reconnect device`、bounded
`wait-for-device`、exact reverse再作成は成功していましたが、同じapp PIDのRust actorは45秒内に
socket lossを観測していませんでした。crash/ANRはありません。accepted-session sshdがlistenerの
process group/descendant snapshot外へ移動・reparentし、接続を保持したままACKできたことが原因です。
Linuxのexact local-port socket owner捕捉を追加し、held OpenSSH sessionでstop/startを実証しました。

iOS standard本体は22画面、production storage 4ケース、native input 12ケース、foundationまで
成功していましたが、Python post-validatorが旧8ケースとの完全一致を要求し、新しい4ケースを
unexpectedとして失敗しました。Swift artifactが固定的に出す12ケースへ期待値を同期し、
欠落・重複・失敗ケースの拒否は維持しました。

同じsourceのiOS ssh
[`35438494982`](https://github.com/phni3j9a/meeterm/actions/runs/35438494982)はfresh CNG buildを含めて成功しました。

## 5bcd58c 診断候補

対象sourceは `5bcd58c1cf1b7420189cd74f5111047c96362c5e` です。

### 一般CI: 成功

[`35440770472`](https://github.com/phni3j9a/meeterm/actions/runs/35440770472)は4 jobすべて成功しました。

- SSH Python driver 199件、Herdr Python driver 6件、App 57件、Expo Doctor 21/21。
- Rust libraryは156件成功・1件ignored、Herdr harnessは4件成功・1件ignored。
- 実OpenSSH/tmux統合は15.55秒で成功。
- 公式Herdr 0.9.0をrunnerの一時領域だけへ取得し、SHA-256
  `4fa1a01158dd8043da92d31b270780b0dcc10603038d9b61cac4d81ab63fb71f`を照合後、
  russh test endpointでのignored統合が21.95秒で成功。
- TypeScript、Expo config、iOS事前検査、Android CNG/build/native unit tests、Rust fmt/Clippyも成功。

ローカルではSSH Python 199件、CI Python 28件成功・1件skip、App 57件、TypeScript、
Expo Doctor 21/21、fixture `--check`、held OpenSSH sessionのstop/start、`git diff --check`を確認しました。
Axiomによる独立レビューはblocking findingなしで承認されました。

Mobile smoke[`35440791321`](https://github.com/phni3j9a/meeterm/actions/runs/35440791321)の
Android fullは`daily_transport_loss_stale / stale_read_only_timeout`で失敗しました。
fixtureはexact portの全sshd owner消失まで確認してstop ACKを返し、`adb reconnect device`、
`wait-for-device`、exact reverse再作成・検証も成功しましたが、45秒内にnative actorへ切断が
伝わりませんでした。[AOSPのlistener実装](https://android.googlesource.com/platform/packages/modules/adb/+/f4965b77c694689c08855076eaf983c8e88646f9/adb_listeners.cpp)では
reverse listenerの削除とaccept済みstreamの寿命は別であり、
[ADB client自身の再起動手順](https://android.googlesource.com/platform/packages/modules/adb/+/refs/heads/main/client/commandline.cpp)は
`wait-for-disconnect`後に`wait-for-device`を行います。次候補ではexact
listener削除・消失確認後、bounded `wait-for-disconnect`を必須にします。deadline、assertion、
keepalive、Rust本番挙動は変更しません。

iOS ssh[`35440792555`](https://github.com/phni3j9a/meeterm/actions/runs/35440792555)はfresh CNG build、
接続、host-key確認、runtime選択、healthy foreground、native input、pre-loss marker、transport loss、
cached/read-onlyまでは成功しました。fixture再start ACK後90秒内にauthoritative Readyへ戻らず、
Swift 885行の`ui_test_failed`で終了しました。crashや入力失敗ではありません。旧724e15fの同じ
app/native経路は成功していますが、この失敗を上書きせず、次候補をfresh buildから再確認します。

同じMobile smokeのiOS standardは、22画面、native input 12ケース、storage 5ケース、fresh processの
foundation生存確認をすべて完了し、`standard_complete`と`teardown_complete`まで記録しました。
その後にXCTest runnerが終了せず、730秒の外側budgetで`xcodebuild_timeout`になりました。
assertion、app crash、runner起動、画面到達の失敗ではありません。最初の失敗段階をこのrunner終了処理と
特定したうえで、次候補を成果物再利用なしのfresh CNG buildから1回再確認します。

## e244746 診断候補

対象sourceは `e24474607a9c01e2ee233353df443fc2e67830d1` です。AndroidのADB reverse listenerを
exact portで削除・消失確認し、`adb reconnect device`後にbounded `wait-for-disconnect`と
`wait-for-device`の両方を実測してから、`--no-rebind`で同じreverse mappingを復元・照合します。
全ADB操作はworkflowが選んだserialへ限定し、切断を観測できなければsmokeをfail closedにします。

### 一般CI: 成功

[`35443112018`](https://github.com/phni3j9a/meeterm/actions/runs/35443112018)は4 jobすべて成功しました。

- SSH Python driver 201件、Herdr Python driver 6件、App 57件、Expo Doctor 21/21。
- Rust libraryは156件成功・1件ignored、Herdr harnessは4件成功・1件ignored。
- 実OpenSSH/tmux統合は15.78秒で成功。
- 公式Herdr 0.9.0をrunnerの一時領域だけへ取得し、SHA-256
  `4fa1a01158dd8043da92d31b270780b0dcc10603038d9b61cac4d81ab63fb71f`を照合後、
  russh test endpointでのignored統合が21.77秒で成功。
- TypeScript、Expo config、iOS事前検査、Android CNG/build/native unit tests、Rust fmt/Clippyも成功。

Android full[`35443114655`](https://github.com/phni3j9a/meeterm/actions/runs/35443114655)は、
exact reverse削除、`adb reconnect device`、bounded `wait-for-disconnect`、同じserialの復帰、
exact reverse再作成を終えた後も`daily_transport_loss_stale / stale_read_only_timeout`で失敗しました。
app crash/ANRはなく、pre-loss markerとfixture stop ACKまでは成功しています。つまりADB transportの
disconnect観測も、既にaccept済みのreverse relayが閉じた証拠にはなりませんでした。

次候補ではdebuggable emulatorをapp/reverse作成前の`adb root`とUID 0で検証します。loss注入では
serial-scoped `wait-for-disconnect`を先にarmし、公開CLIの`adb unroot`でadbdを再起動します。
固定restart応答、disconnect、同じserial、shell UID 2000、exact reverse復元のいずれかを確認できなければ
fail closedにします。adbdが所有するaccept済みrelayをプロセス終了で閉じるfixture変更だけで、
45秒deadline、assertion、keepalive、本番Rust挙動は変更しません。

### iOS acceptance: 成功

iOS ssh[`35443116071`](https://github.com/phni3j9a/meeterm/actions/runs/35443116071)はfresh CNG buildから
成功しました。fixture stop/start、pre/post marker各1回、同じpane/PID、同じnative terminal ID/handle、
cached read-only、loss中inputなし、別pane非混入を固定artifactで確認しました。Simulator logは
native readinessと`MEETERM_SMOKE_FIRST_FRAME_METAL`を記録しています。初期画面、native terminal/keyboard、
明示切断画面の画像をdownloadして実見し、表示破綻やcrash画面がないことを確認しました。

iOS standard[`35443247004`](https://github.com/phni3j9a/meeterm/actions/runs/35443247004)も成果物再利用なしの
fresh CNG buildから成功しました。22画面、native input 12ケース、storage 5ケース、fresh processの
foundation生存、native readiness、Metal first frame、XCTest終了コード0を確認しました。
`recovery-progress`、`recovery-exhausted`、`recovery-mismatch`、`herdr-recovery-confirm`、terminal、
foundationの画像をdownloadして実見し、保持画面と案内/action、CJK/emoji/ANSI描画に目立つ欠けが
ないことを確認しました。前候補の`teardown_complete`後のrunner終了ハングは再発しませんでした。

## f5245cd 診断候補

対象sourceは `f5245cde8947ee0726dd88b40fb208e89cd804fd` です。e244746から変わったのは
Android acceptance fixtureとその回帰・運用文書だけで、app/native/iOS build inputとiOS assertionは
変えていません。そのため上記iOS fresh acceptanceを再利用せず証拠として引き継ぎ、一般CIとAndroid fullを
このsourceで再実行します。

Androidは対象serialのdebuggable emulatorを`adb root`の固定応答とUID 0で先に検証します。
loss注入時はexact reverseを削除・消失確認し、serial-scoped `wait-for-disconnect`を先にarmしてから
公開CLIの`adb unroot`でadbdを再起動します。disconnect、同じserialの復帰、shell UID 2000、
`--no-rebind`のexact mappingを順に照合し、root非対応、応答/UID不一致、waiter timeoutはfail closedです。
[AOSPのroot/unroot設計](https://android.googlesource.com/platform/packages/modules/adb/+/refs/heads/main/docs/dev/root.md)と
[ADB CLI reference](https://android.googlesource.com/platform/packages/modules/adb/+/refs/heads/main/docs/user/adb.1.md)に沿い、
`setprop`や`killall`は使いません。Axiomのadvisor/reviewerはこのserial境界、Popen lifecycle、cleanup、
固定artifactをblocking findingなしで承認しました。

ローカルではSSH Python 210件、Herdr Python 6件、CI Python 28件成功・1件skip、Android driver 110件、
`py_compile`、`git diff --check`を確認しました。

### 一般CI: 成功

[`35445993609`](https://github.com/phni3j9a/meeterm/actions/runs/35445993609)は4 jobすべて成功しました。

- SSH Python driver 210件、Herdr Python driver 6件、App 57件、Expo Doctor 21/21。
- Rust libraryは156件成功・1件ignored、Herdr harnessは4件成功・1件ignored。
- 実OpenSSH/tmux統合は13.47秒で成功。
- 公式Herdr 0.9.0のSHA-256
  `4fa1a01158dd8043da92d31b270780b0dcc10603038d9b61cac4d81ab63fb71f`を照合後、
  russh test endpointでのignored統合が21.61秒で成功。
- TypeScript、Expo config、iOS事前検査、Android CNG/build/native unit tests、Rust fmt/Clippyも成功。

Android full[`35446001113`](https://github.com/phni3j9a/meeterm/actions/runs/35446001113)は、
root UID 0、exact reverse削除、serial-scoped disconnect waiterのarmまで終えた後、
`transport_adbd_unroot / adbd_unroot_not_confirmed`でfail closedしました。app crash/ANRはなく、
pre-loss markerも完了しています。実行時の非機密stdoutを保存しない設計のため余分な行の内容は
断定できませんが、実装がAOSP定義の応答だけでなくstdout全体の完全一致を要求していたことを確認しました。

次候補では、reverseを変更する直前にもroot UID 0を再確認し、`adb unroot`のstdoutは
`restarting adbd as non root`がtrim後の独立した行として含まれることを要求します。これによりCLIの
補助出力は許容しますが、空応答、`adbd not running as root`、未知の応答は固定カテゴリだけを成果物へ
記録してfail closedにします。disconnect waiter、同じserial、UID 2000、exact reverse、45秒deadline、
acceptance assertion、本番コードは変更しません。

## 8119747 候補

対象sourceは `8119747072c60098518075a15e9f5958faa4ae57` です。f5245cdから変わったのはAndroid
acceptance driverの`adb unroot`応答判定、追加回帰、運用文書だけです。app/native/iOS build inputと
iOS assertionは変えていないため、e244746のfresh iOS ssh/standard acceptanceと実見済み画像を
この候補へ引き継ぎます。

`adb unroot`のstdout全体ではなく、AOSP定義の`restarting adbd as non root`がtrim後の独立行として
含まれることを要求します。reverse変更前に同じserialのroot UID 0も再確認します。補助行は許容しますが、
substringだけの一致、空応答、非root応答、未知の応答は固定カテゴリのみ記録してfail closedです。
Axiomの独立reviewerは、行境界、UID照合順、waiter lifecycle、stdout非記録、既存deadline/assertionに
blocking findingなしと判定しました。

ローカルではAndroid driver 113件、SSH Python全体213件、Herdr Python 6件、CI Python 28件成功・
1件skip、`py_compile`、`git diff --check`を確認しました。実OpenSSH/tmux統合は15.20秒で成功し、
disposable fixtureの実認証・tmux checkも20回連続で成功しました。

### 一般CI: 成功

[`35448204439`](https://github.com/phni3j9a/meeterm/actions/runs/35448204439)は4 jobすべて成功しました。

- SSH Python driver 213件、Herdr Python driver 6件、App 57件、Expo Doctor 21/21。
- Rust libraryは156件成功・1件ignored、Herdr harnessは4件成功・1件ignored。
- 実OpenSSH/tmux統合は14.14秒で成功。
- 公式Herdr 0.9.0のSHA-256
  `4fa1a01158dd8043da92d31b270780b0dcc10603038d9b61cac4d81ab63fb71f`を照合後、
  russh test endpointでのignored統合が21.23秒で成功。
- TypeScript、Expo config、iOS事前検査、Android CNG/build/native unit tests、Rust fmt/Clippyも成功。

同じsourceのpush run
[`35448202211`](https://github.com/phni3j9a/meeterm/actions/runs/35448202211)は、実OpenSSH test開始前の
fixture cleanupで`OpenSSH fixture listener identity changed`となりました。同時に走った上記PR runと
ローカル実行は成功しており、Rust/app assertionの失敗ではありません。この失敗も成功runで上書きせず
診断記録として残します。

### Android acceptance

初回Android full
[`35448212593`](https://github.com/phni3j9a/meeterm/actions/runs/35448212593)はfresh CNG/release build、
install、launch、native readiness/first frame、29画面の撮影を終えましたが、実SSH driver開始直後の
同じfixture identity診断で終了しました。SSH/transport artifactはまだ生成されず、app PID 3222は生存、
ANR/crash画面はありません。一般CIのexact-source成功とローカル20回のfixture checkを確認したうえで、
同一sourceのAndroid fullを1回だけ再実行します。再発時は追加retryせずfixtureを修正します。

再実行
[`35449265712`](https://github.com/phni3j9a/meeterm/actions/runs/35449265712)ではfixture identity検査を通過し、
fresh CNG/release build、install、launch、native readiness/first frame、29画面、実SSH接続、pre-loss marker、
fixture stop ACKまで完了しました。ADB側もroot UID再確認、exact reverse削除、disconnect waiter、独立行の
unroot応答、adbd再起動、disconnect、同じserial、shell UID 2000、exact reverse復元をすべて固定eventで
確認しました。それでも`daily_transport_loss_stale / stale_read_only_timeout`となり、45秒内にnative actorへ
SSH EOFが届きませんでした。app PID 3415は生存し、ANR/crashはありません。したがってadbdの再起動は
ADB transportの切断証拠にはなっても、host側ですでにaccept済みのreverse relayを閉じる境界ではありません。
同じ方式の追加retryは行いません。

### 次候補: host ADB server境界

[AOSPのADB client実装](https://android.googlesource.com/platform/packages/modules/adb/+/refs/heads/main/client/commandline.cpp)は
`kill-server`をhost server終了、`root`/`unroot`をadbd再起動として別操作にしています。
[host service実装](https://android.googlesource.com/platform/packages/modules/adb/+/HEAD/adb.cpp)ではkill要求へOKを返した後、
process exitによるsocket closeに依存します。また
[reverse service仕様](https://android.googlesource.com/platform/packages/modules/adb/+/HEAD/docs/dev/services.md)では、
reverseのlocal endpointはdevice側、remote endpointはhost側です。今回の実測と合わせると、accepted relayを
確実に破棄するにはrelayを保持するhost ADB serverのprocess exitが必要、というのが現時点の推論です。

次候補はAndroid fullのephemeral GitHub-hosted単一emulator jobだけで明示opt-inします。default
`127.0.0.1:5037`、選択済みemulator 1台、shell UID 2000、他transport/reverseなしを操作前とkill直前に
fail closedで確認します。exact reverseを削除・消失確認後、host `adb kill-server`を実行し、ADB commandを
挟まずraw TCPで3回連続のport閉鎖を要求します。途中でlistenerが戻ればraceとして失敗します。その後
`adb start-server`、同じserialだけの復帰、同じUID、空のreverse list、exact reverse再作成を照合します。
停止開始後の失敗にはbounded cleanupを行い、固定event以外のstdout/device一覧/serialをartifactへ出しません。
Axiom advisorはglobal operationをlocal/shared runnerで拒否するこの境界を条件付きで承認しました。45秒deadline、
keepalive、assertion、Rust/app/native本番コードは変更しません。

## e12e6cc 診断候補

対象sourceは `e12e6cc07cab1125d259b910b0625537e7fa9f43` です。host ADB server境界の実装と
fail-closed guard、cleanup、22件のfocused回帰をAxiom advisor/reviewerがblocking findingなしで
承認しました。ローカルではSSH Python 215件、Herdr Python 6件、CI Python 28件成功・1件skip、
`py_compile`、`git diff --check`を確認しました。

一般CI[`35452192770`](https://github.com/phni3j9a/meeterm/actions/runs/35452192770)は4 jobすべて成功しました。
Rust/App/Expo、実OpenSSH/tmux、公式Herdr 0.9.0 ignored integration、Android CNG/build/native unit、
iOS preflightを通過しています。

Android full[`35452201693`](https://github.com/phni3j9a/meeterm/actions/runs/35452201693)はfresh CNG/release build、
install、launch、native readiness/first frame、29画面、実SSH、pre-loss marker、fixture stop ACKまで完了しました。
host ADB serverについても、single-emulator/default endpoint guard、exact reverse削除、kill要求、raw
`127.0.0.1:5037`閉鎖、server再開、同じserial/UID、空reverse、exact mapping復元の全固定eventを確認しました。
それでも`daily_transport_loss_stale / stale_read_only_timeout`となり、45秒内にnative actorへSSH EOFは届きませんでした。
app PID 3235は生存し、crash/ANRはありません。したがって前節の「host ADB server process exitでaccepted relayを
確実に破棄できる」という推論は、このemulator/runtimeでは成立しませんでした。同方式のretryは行いません。

### 次候補: Android Emulator host-loopback直結

[Android公式のemulator network仕様](https://developer.android.com/studio/run/emulator-networking-address)は、
`10.0.2.2`を開発hostのloopback `127.0.0.1`へのspecial aliasと定義しています。Android driverだけ
fixtureの接続フォームhostをこのaliasへ置換し、ADB reverseの作成/削除、adbd再起動、host ADB server再起動を
全廃します。fixture sshdは従来どおりhost `127.0.0.1:<ephemeral port>`へbindし、exact listener/session ownerの
終了を確認してstop ACKを返します。intermediate relayがないため、そのsession socket終了をappのTCP EOF境界として
直接検証します。

driverはcredential入力前に`emulator-<port>` serial、`adb devices -l`上の単一ready transport、空の
serial-scoped reverse listを要求し、loss後にもreverseが空であることを再確認します。mappingの作成・削除は
行いません。bounded zero-I/O probeでexact alias/portの到達性をcredential入力前に確認し、到達不能は固定理由で
fail closedにします。fixture環境のpublished hostは`127.0.0.1`以外を拒否します。host-key fingerprintは同じfixture keyを
明示確認するため弱めません。tmux/session/shell、app PID、
native handle、pane、45秒、cached/read-only、loss中inputなし、pre/post markerのassertionと本番コードは変更しません。
