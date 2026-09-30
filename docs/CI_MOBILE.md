# Mobile診断の実行ガイド

実行するかどうかは[TESTING.md](TESTING.md)で決めます。この文書は実行時の手順です。
Android full / iOS standardは毎回の変更に必要なゲートではありません。
開発APKの自動生成は[ANDROID_RELEASES.md](ANDROID_RELEASES.md)を参照してください。

## 使用する環境

Devin Cloudの常駐セッションで、必要なOSだけ実行します。

| OS | セッション |
| --- | --- |
| Android / Linux KVM | [9429c00e8cc14fb2b140b3e23bb28ec1](https://app.devin.ai/sessions/9429c00e8cc14fb2b140b3e23bb28ec1) |
| iOS / macOS Apple Silicon | [7a32a4e6ed984961b5194e22feeba407](https://app.devin.ai/sessions/7a32a4e6ed984961b5194e22feeba407) |

Mainは `scripts/ci/devin-cloud.py` からDevin CLIの `devin acp --cloud` を使います。
認証は `devin auth login`。REST API用の鍵は不要です。新規セッションはSWE-2 Maxを
明示指定し、使用不能ならその理由を報告します。別モデルで代用しません。

```sh
python3 scripts/ci/devin-cloud.py list
python3 scripts/ci/devin-cloud.py new --platform macos --prompt-file /tmp/meeterm-run.md --wait 60
python3 scripts/ci/devin-cloud.py send <session-id> --prompt-file /tmp/meeterm-run.md --wait 60
python3 scripts/ci/devin-cloud.py wait-evidence evidence/ios-<date> --after <previous-head> --timeout 5400
python3 scripts/ci/devin-cloud.py status <session-id> --messages 3
```

`new` の既定は `devin-swe-2-max`、repoは `phni3j9a/meeterm` です。
`--wait` 終了後もcloud処理は継続します。長い処理は `wait-evidence` で待ち、短い間隔の
status確認を繰り返しません。送信前のevidence headを `--after` に渡すと早い完了も拾えます。
idleでもsession statusがrunningのことがあるため、完了は新しい証跡commitと結果で判断します。
ACPのモデル識別子は外部サービス依存です。変更された場合は失敗を調べてからdriverを直します。

## Suiteを選ぶ

| 経路 | 実行範囲 |
| --- | --- |
| Android foundation | `scripts/ci/android-smoke.sh`。用意したRelease APKのinstall/launch/native frame/no-crash |
| Android SSH/full | `scripts/ssh/android-smoke.py`を隔離SSH fixtureから実行。保存・設定・pane操作・theme・復旧などを含む大きな診断 |
| Android presentation | `scripts/ci/android-screen-fixtures.py`。公開のseeded画面を観測 |
| iOS `native` | production保存とnative入力。起動/描画の総合検証ではない |
| iOS `standard` | 保存・入力・画面巡回・theme・fresh native foundation。広範な表示/描画診断 |
| iOS `ssh` | 実SSH、host key、runtime選択、入力、switch、foreground/transport-loss復旧 |
| iOS `polish-navigation` | 検索・keyboard・sheet・back操作とfoundation |
| iOS `polish` | 上記操作と追加の表示状態 |
| iOS `forms` / `names` / `full` | フォーム、名前操作、旧全操作をそれぞれ調べる診断 |

iOSは `MEETERM_IOS_SUITE=<suite> scripts/ci/ios-smoke.sh` で実行します。
scriptの既定値は互換性のため `standard` のままですが、依頼にはsuiteを明記します。
`MEETERM_IOS_PROFILE=compact-xl` は小画面・大きい文字の任意診断です。

画面名やtest件数をこの文書へ複製しません。現行manifestは
[MeetermSmokeUITests.swift](../scripts/ci/MeetermSmokeUITests.swift)と
[android-screen-fixtures.py](../scripts/ci/android-screen-fixtures.py)、
suiteの完了条件は各driverにあります。suiteを選んだらそのassertionを省略しません。
`ssh` も複数の接続操作を含むため、起動だけの短いsmokeとは区別します。

## Buildと再利用

依頼には、対象SHA・OS・suite/操作・fresh buildか再利用か・evidence branchを指定します。

1. 専用runner checkoutが他作業で使われていないことを確認し、対象SHAを取得する。
2. `npm ci` と対象toolchainの確認。iOSは `scripts/ci/ios-typecheck.sh` を先に実行。
3. `EXPO_PUBLIC_MEETERM_SMOKE=1` を指定して対象OSのCNGを生成し、Release構成をbuildする。
4. Emulator/Simulatorを起動し、対象scriptでinstall・操作・結果収集を行う。
5. 成否にかかわらずsanitizedな観測結果をevidence branchへpushする。

`android/` と `ios/` は生成物です。必要な変更はapp config、plugins、native moduleへ戻します。
Node/Rust/Androidの固定値は `.nvmrc`、Rust toolchain、ci.ymlが基準です。
Android foundationのAPKは
`artifacts/android-emulator-observability/app-release.apk` に用意します。
AndroidのCNG/buildコマンドはci.ymlとandroid-release.ymlを参照してください。

iOSは同一セッション・同一commit・同一toolchain/CPU/構成のpristine
`build-for-testing` productsに限り、別suiteで再利用できます。
[scripts/ci/ios-test-products.py](../scripts/ci/ios-test-products.py)で一致を検証し、
fixture環境変数は実行ごとの一時コピーにだけ注入します。
app/native/test/build入力を変えたら新しいbuildが必要です。
元buildと再利用runを記録し、再利用をfresh buildと呼びません。

Simulatorは署名なしで実行します。証明書・provisioning profile・Apple秘密情報を要求しません。
Keychain試験はproduction moduleをimportするapp-hosted targetで行い、使い捨てのSimulator
app本体にXML/DER entitlementを埋め込みます。別processのUI runnerへ付けても代用できません。
[scripts/ci/ios-inject-ui-test.py](../scripts/ci/ios-inject-ui-test.py)と
[ios-strip-storage-rust-link.py](../scripts/ci/ios-strip-storage-rust-link.py)を使用します。
実機/TestFlightの署名・配布は別作業です。

## Runtime結果と画像

foundationの成功にはinstall、launch、native readiness、実Rust snapshotのfirst frame、
no-crashが必要です。環境準備・静的label・画像の存在だけでは成功にしません。
iOSのMetal markerとSimulator専用CoreGraphics markerを区別します。
Simulatorは実機GPU・IME・font parityの証拠ではありません。

smoke buildと明示URL `meeterm://smoke?screen=<name>` で公開fixtureを開けます。
必要なときだけ `&app=<theme>&terminal=<theme>` をこの順で指定します。
foundation URLは `meeterm://foundation?foundation=1` です。
通常起動ではfixtureを有効にせず、端末データはRust/nativeで作ります。
seeded画面を実接続・保存・入力操作の証拠として報告しません。

実SSH試験は隔離fixtureを使い、host keyとremote input markerを検証します。
Android fullとiOS sshのtransport-loss試験はtmuxが対象です。Herdr実接続はRust/russhの
ignored integrationで確認します。foreground復帰と意図的なtransport lossを混同しません。
ユーザーのsshd・tmux・Herdrを停止したり、秘密情報をログへ出したりしません。

## 成果物と失敗調査

- Android: `artifacts/android-emulator-observability/`
- iOS: `artifacts/ios-simulator-observability/`
- 保存先: `evidence/<platform>-<date>` のorphan branch。大きなAPKは必要時だけ含めます。

launch後は画像とsanitized native logを採取します。到達しなかった場合はunavailable理由を
保存し、画像を捏造しません。秘密欄の画像、生XCTest/xcresult、入力値、clipboardや端末の
生内容はアップロードせず、runnerの一時領域にとどめます。

```sh
git fetch origin evidence/ios-YYYYMMDD
mkdir -p /tmp/meeterm-evidence-YYYYMMDD
git archive origin/evidence/ios-YYYYMMDD | tar -x -C /tmp/meeterm-evidence-YYYYMMDD
```

Mainは報告対象の画像を実際に開きます。全画面・両OS画像の一律取得は不要で、変更した画面と
確認対象OSに絞ります。PRにはsource/suite・結果・成果物・確認できなかった点を簡潔に残します。

最初の失敗をbuild、Simulator、保存/入力、表示、SSH、描画に分け、該当ログから調査します。
同じ失敗をblind retryせず、assertionを飛ばしたりdeadlineを延ばしたりしません。
iOS driverは最上位XCTest結果が出た後の終了ハングを判別しますが、結果のないtimeoutは失敗です。
以前のfull失敗を別suiteの成功で上書きしません。

## Runner固有の注意

- Apple SiliconのSimulatorは `ARCHS=arm64`、Rustは `aarch64-apple-ios-sim`。
- `SIMCTL_CHILD_TZ=UTC` を設定し、log収集開始時刻のtimezoneを揃えます。
- Maven Centralが403の場合はrunnerのnetwork policyを確認します。既存Linux sessionには
  Google公式mirrorを使うGradle init設定があります。無関係な依存更新で回避しません。
- 同じVMにはprofileや古い成果物が残ります。専用のtest app/dataだけを初期化し、
  raw XCTest productsと再利用対象のpristine productsを混ぜません。
- runtime不足、toolchain drift、セッション消失は環境の失敗として報告します。
  新規sessionの現在の状態を調べ、必要なものだけ準備します。

過去の詳細結果は[検証履歴](evidence/testing-method-validation-history.md)と
[daily-use履歴](evidence/daily-use-validation-history.md)を参照してください。
