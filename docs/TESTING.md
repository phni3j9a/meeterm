# 標準テスト手順

meetermの修正は **短いチェック → 対象を絞った操作確認 → 最終の全体検証** の順に進めます。
失敗した長いテストをそのまま繰り返すことを標準にしません。
この手順は `CI` と `Mobile smoke` ワークフローに実装しています。
製品の合格条件と実機検証の境界は [CI_MOBILE.md](CI_MOBILE.md) を維持します。

## 変更に合わせて選ぶ

| 変更 | 最初の確認 | 対象確認 | 最終確認 |
| --- | --- | --- | --- |
| Pythonのテストドライバ・成果物処理 | 対象Python回帰テスト | 該当OS・該当suite | 影響する全体テスト |
| iOSの自動操作・Swift入力テスト | `ios-typecheck.sh` | `forms` / `native` / `names` | iOS `full` |
| 接続フォームのUI | TypeScript・Swiftの該当チェック | iOS `forms`、Androidの操作確認 | 両OS `full` と画像の実見 |
| Rust・native adapter・依存関係・CNG/build設定 | Rust/型/単体/生成設定の該当チェック | 対象native操作 | fresh CNGから両OS `full` |
| ドキュメントのみ | 記述・リンク・実際のコマンドとの整合 | 不要 | nativeテストの再実行は不要 |

アプリ・nativeコードのpushは従来どおり自動の全体Mobile検証を起動します。
テスト・スクリプト・CI設定だけの修正では自動の全体Mobile検証を起動せず、一般CIの短いチェックを
先に確認して、対象suiteを明示して実行します。Mobileワークフローやbuild設定を変更した場合も、
対象確認の後に手動でfresh CNG・両OSの全体検証を実行します。
ドライバ変更でも、対象suiteが成功しただけで全体の受入を成功扱いにはしません。

## 1. 長いビルドの前に確認する

Linuxでも実行できる例:

```sh
npm run typecheck
python3 -m unittest discover -s scripts/ssh -p 'test_*.py'
python3 -m unittest discover -s scripts/ci -p 'test_*.py'
git diff --check
```

変更に無関係なテストを毎回すべて実行する必要はありません。
例えば成果物の再利用処理だけなら `python3 scripts/ci/test_ios_test_products.py` を先に実行します。

macOSでXcodeを選択した環境では:

```sh
scripts/ci/ios-typecheck.sh
```

このチェックはUI XCTestとネイティブ入力関連のSwiftを型チェックします。
Expo生成、CocoaPods、Rustビルド、Simulator起動は行いません。
削除した引数が呼び出し側に残るようなコンパイルエラーを、長いアプリビルドの前に検出します。
一般CIで自動実行し、MobileのiOSビルドもこの成功を前提に開始します。

`ClientStoreTests.swift` は実アプリのproduction moduleへ依存するため、この短いチェックの
対象ではありません。ストレージのコンパイル・Keychain動作はアプリビルドと `native` で確認します。
短い型チェックの成功を、iOSアプリ全体のコンパイル成功と読み替えないでください。

## 2. 対象を絞って実行する

`Mobile smoke` の手動実行には次の入力があります。

| 入力 | 値 | 用途 |
| --- | --- | --- |
| `platform` | `both` / `android` / `ios` | 調査するOSを選択。既定はboth |
| `ios_suite` | `forms` | 公開フィールド入力、認証方式切り替え、保存設定のフォーム操作 |
| `ios_suite` | `native` | production保存4件とネイティブ入力7件 |
| `ios_suite` | `names` | 実SSH接続後のworkspace/pane作成・名前変更・終了操作 |
| `ios_suite` | `full` | 実SSH/tmux、日常操作、handoff、再接続、最後のfresh foundation。既定値 |
| `ios_build_run` | 空、または実行ID | 空ならfresh build。同一commitのビルド済み成果物を指定すると診断用に再利用 |

例（現在のブランチでiOSフォームを確認）:

```sh
test_ref="$(git branch --show-current)"
gh workflow run mobile-smoke.yml --ref "$test_ref" -f platform=ios -f ios_suite=forms
gh run list --workflow mobile-smoke.yml --branch "$test_ref" --limit 5
```

`forms` と `native` はSSH fixtureを起動せず、秘密鍵・パスワードを入力しません。
`forms` は本番画面と共通の操作helperを使い、固定の公開テスト値だけで操作後にキャンセルします。
各suiteは明示したtest IDだけを実行し、新しい完了記録を要求します。
XCTestの終了コードが0でも、期待するケースや完了記録が不足すれば失敗します。
focused結果は `ios-forms-validation.txt` / `ios-native-validation.txt` へ保存し、
全体受入の `ios-validation.txt` と区別します。フォームの画像は専用の3チェックポイントを
確認し、`native` ではUI画像を要求しない理由を明示します。全体画面の欠落と誤分類しません。

`names` は実SSH fixtureを使い、fullと同じ鍵入力・接続・ホスト鍵確認・名前操作helperを実行します。
公開鍵方式から接続し、認証方式の切り替え・認証情報保存の確認はfullに残します。
保存・入力の単体テストや、reconnect・copy・設定のシナリオはこのscopeでは実行しません。
`names_complete` と新しい `case=names result=passed`、xcodebuild正常終了を要求し、
`ios-names-validation.txt` に結果を残します。fullのdaily/foundation合格記録は出しません。
長い生成名の編集問題はこのsuiteで先に確認し、その後fullを実行します。


## 3. ビルド済み成果物を再利用する

iOSは次のジョブに分かれています。

1. `iOS driver / fast typecheck` — 安価なSwiftチェック。
2. `iOS / fresh CNG build` — 新しいcheckoutからCNG生成・アプリとテストをビルド。
3. `iOS Simulator / <suite>` — 別runnerでSimulator起動・インストール・対象テスト。

buildとruntimeの制限時間を分離し、長いビルドが操作テストの時間を消費しない構成です。
全体テスト内の保存＋UIの合計30分という上限は延長しません。
`forms` と `names` はテスト実行全体を15分、`native` は10分に制限します。
フォームの初回実測ではUI操作だけで約448秒かかり、完了記録は出たものの、
XCTestの起動準備・終了処理を含む10分ではプロセスが終了しませんでした。
この実測を理由にformsのみ15分へ変更し、起動と終了の時間も記録します。
完了記録だけでは成功扱いにせず、引き続きxcodebuildの正常終了を要求します。

ビルド成功時に `ios-test-products` artifactを保存します。
**同一commitの別suiteや環境要因の再現確認**では、その実行IDを指定できます。
`BUILD_RUN_ID` は実際の数値IDに置き換えてください。

```sh
gh workflow run mobile-smoke.yml --ref "$test_ref" \
  -f platform=ios -f ios_suite=native -f ios_build_run=BUILD_RUN_ID
```

取得元は同じリポジトリの `Mobile smoke` に限定し、APIのcommitと現在のcheckout、manifestの
commit・Xcode version/build・CPU構成・Release Simulator構成・SHA-256を照合します。
合わなければ停止します。自動で「最新の成功した古いアプリ」を選ぶことはしません。

成果物はfixtureの環境変数を注入する前に作り、`.app` / `.xctest` の実行権限とsymlinkを
保持するtarへ格納します。runtimeの環境注入は試行ごとの一時コピーだけに行います。
秘密を含み得るraw XCTestログ・xcresult・fixtureファイルはartifactへ含めません。
artifactの保持期間は7日です。期限切れなら新しくビルドします。

**Swift・アプリ・テストコードを変更した場合は新しいビルドが必要です。**
この仕組みは変更後のコードを古いバイナリで検証するものではありません。
同一ソースの操作再実行と、ビルド成功後に別suiteを確認する際の再ビルドを省きます。
変更のたびの待ち時間は、最初の型チェックと対象suiteによる早期発見で抑えます。

## 4. 失敗時の調べ方

| Artifact | 確認するもの |
| --- | --- |
| `ios-build-observability` | CNG/buildログ、使用Xcode、起動前であることの診断 |
| `ios-simulator-observability` | suite別の合否、段階・時間、安全な画像、sanitized nativeログ |
| `ios-test-products` | 同一ソース再利用用のtarとmanifest。実行後のログは含まない |

例えば `RUN_ID` を実行IDに置き換え、新しい保存先へ取得します。

```sh
gh run download RUN_ID --name ios-simulator-observability --dir /tmp/meeterm-evidence-RUN_ID
```

1. 最初に失敗した境界を特定します。型チェック、CNG/compile/link、Simulator起動、
   保存/入力、画面操作、SSH、最後の描画を分けます。
2. 対応するartifactの固定診断、`ios-ui-stages.txt`、`ios-ui-timing.txt` を読みます。
   `ios-ui-clock.txt` の時刻アンカーとrunner診断の開始時刻・所要時間、
   `teardown_started` / `teardown_complete` から、操作前後の待ちも切り分けます。
   スクリーンショットや動画は実際に開き、撮影できた範囲だけを根拠にします。
3. 原因の仮説を一つに絞り、具体的なログや小さな再現で確かめてから修正します。
   証拠不足なら、次の一回で必要な状態が分かる診断を先に追加します。
4. 同じ失敗を理由なく繰り返さず、修正に対応する短いチェック・suiteから再実行します。

`full` / `names` の接続失敗では `ios-ui-connection-diagnostics.txt` も確認します。
保存済みprofileとfixtureの一致フラグ、および失敗後のstrict SSH probe結果を比較します。
probe成功は事後のfixture認証が正常という証拠であり、UI入力した鍵の一致や失敗時点の
応答速度までは証明しません。診断が取得できない場合も、元のUI失敗を維持します。

自動操作では次を標準にします。

- 安定したaccessibility IDで対象を特定し、表示・操作可能状態を確認する。
- キーボードを除いた表示領域と現在の対象位置からスクロール方向・距離を決める。
- 短い文字入力は正確な値を読み返してから送信する。値は実動確認済みの要素属性から読む。削除後の空欄確認は即時の完全一致確認から始める。長い名前の削除は全選択＋1回のDeleteを使う。すでに編集メニューが表示されていればそれを使い、不要な長押しでメニューを閉じない。
- 固定sleepや闇雲な追加retryで成功させない。待機は状態条件と上限を持つ。
- 失敗時の固定診断を先に保存し、画面取得の失敗で原因を隠さない。
- 秘密の入力値やrawリモートエラーを診断へ出さない。フォーム撮影は秘密入力前だけ。
- iOSのコピー内容はhost側の既存observerで検証し、Runnerからの読み取り許可ダイアログで止めない。

終了コード・テスト選択・完了markerを弱めたり、失敗箇所を飛ばした結果を成功扱いにする変更はしません。
タイムアウトやretry上限の変更は、実測と理由を記録して判断します。

## 5. 最後の受け入れ

製品/nativeの変更を完成と報告する前に、最新ソースをfresh CNGから両OSで検証します。

```sh
gh workflow run mobile-smoke.yml --ref "$test_ref" -f platform=both -f ios_suite=full
```

`ios_build_run` は指定しません。再利用でfullを実行できても、その結果は診断用です。
両OSの画像をダウンロードして実際に確認し、native readiness・first frame・no-crash、
実SSH/tmux操作など既存の合格条件が揃ったことを確認します。
MetalとSimulator専用ソフトウェア描画を区別し、実機GPU・フォント・日本語IMEまで
確認したとは扱いません。

記録にはcommit、run URL、suite、fresh/reused、実行結果、実見した画像、残る制約を含めます。
`forms` の成功、`native` の成功、全体の成功は別々に記載してください。

実装の根拠: Appleの [build-for-testing / test-without-building](https://developer.apple.com/library/archive/technotes/tn2339/_index.html) と
GitHubの [workflow artifact](https://docs.github.com/en/actions/concepts/workflows-and-actions/workflow-artifacts) を利用しています。

## 導入時の検証記録

`cb69a17` では次の結果を確認しました。

| 実行 | 結果 | 確認した範囲 |
| --- | --- | --- |
| [一般CI](https://github.com/phni3j9a/meeterm/actions/runs/34548009401) | 成功 | Rust/実SSH、JS/Expo、Android native、macOS Swift/collector事前チェック |
| [forms・fresh build](https://github.com/phni3j9a/meeterm/actions/runs/34548011799) | 成功 | 別runnerへの復元、フォーム操作、xcodebuild正常終了、証拠収集、3枚の画像を実見 |
| [native・同一ソース再利用](https://github.com/phni3j9a/meeterm/actions/runs/34549149649) | 成功 | build省略、復元、production保存4件、native入力7件、証拠収集 |
| [Android full・fresh CNG](https://github.com/phni3j9a/meeterm/actions/runs/34550694155/job/103112788927) | 成功 | 日常操作69完了記録、native ready/first frame/no-crash、4枚の画像を実見 |
| [iOS full・fresh CNG](https://github.com/phni3j9a/meeterm/actions/runs/34550694155/job/103115483648) | 未完了 | build・復元・保存4件・入力7件成功。pane名変更時のSelect All操作で失敗。証拠収集は成功 |

同じ`cb69a17`の初回buildジョブは13分21秒でした。native再利用実行ではこの工程が
省略されています。Simulatorの起動とテスト実行は毎回必要で、native runtimeジョブは
17分32秒、forms runtimeジョブは23分6秒でした。15分/10分のsuite上限は
xcodebuildテスト実行部分を対象とし、Simulator起動を含むruntimeジョブ全体とは異なります。

formsのxcodebuild実行は805.4秒（15分枠内）でした。内訳はXCTest準備が約143.1秒、
setupからteardown完了まで約645.8秒、終了までの残りが約16.4秒です。
この実行では終了処理の停滞は見られません。runner条件による所要時間の変動があるため、
初回の448秒という操作時間だけで全実行の上限を決めないようにします。

以下は導入中に見つけて修正した問題の記録です。

- `a398a68` の [Swift事前チェック](https://github.com/phni3j9a/meeterm/actions/runs/34544837219/job/103095111270) はmacOS上で成功しました。最初の試行で不足していたXCTestのSwift検索パスは、成功済みの実アプリビルドと同じ設定へ修正しています。
- [最初のforms実行](https://github.com/phni3j9a/meeterm/actions/runs/34544845213) は、macOSの `/var` と `/private/var` の違いを成果物処理の回帰テストが検出し、アプリビルド前に停止しました。両表記を変換する修正と、シンボリックリンク経由のroundtripテストを追加しています。
- `0e345c9` の [formsビルド・実行](https://github.com/phni3j9a/meeterm/actions/runs/34545053532) では事前チェック35秒、fresh buildジョブ11分44秒、別runnerへの復元が成功しました。フォームは認証切替・保存設定・キャンセルまで完了記録が出ましたが、xcodebuildの600秒上限で失敗しています。3枚のフォーム画像をダウンロードして実見しました。
- 同じソースの [native再利用実行](https://github.com/phni3j9a/meeterm/actions/runs/34546054376) ではbuildが省略され、成果物復元・production保存4件・native入力7件が成功しました。その後、collectorがmacOS Bash 3.2の空配列展開で失敗しました。この互換修正に加え、collector回帰をmacOSの事前チェックにも組み込み、修正後の実行で成功を確認しました。ジョブ全体の成功とは区別します。
- iOS fullは67文字の生成pane名を全選択する操作で停止しました（`source_line=987`）。失敗画面と操作動画、固定stage/時刻診断を保存しました。`daily_complete` と最後のfresh foundationは未到達です。利用者の区切り依頼後、再開依頼を受けてnames限定suiteで調査を再開しました。
- テスト方法の導入・文書化と限定検証は完了していますが、日常利用全体の受入は未完了です。focusedの操作記録や再利用成功は、fullの合格を意味しません。

### 名前操作の限定検証

`c37b046` の一般CIとSwift/native appビルドは成功しました。
[最初のnames実行](https://github.com/phni3j9a/meeterm/actions/runs/34555370667)は、
起動前の実SSHチェックが約1秒で成功した一方、Simulator起動後のfixture準備待ちで停止しました。
XCTestには到達しておらず、画像は撮影できていません。fixture内部の停止箇所は未特定です。

同じバイナリを[別runnerで一度再利用](https://github.com/phni3j9a/meeterm/actions/runs/34556929048)し、
fixture準備は通過しました。表示済みSelect Allを使う分岐を実行し、workspace名の削除後は
固定診断・実見した画像とも空欄でした。しかし5秒の値待機がtimed outとなり、その後の
xcodebuildも900秒で終了できず失敗しました。`names_complete`は出ていません。
この結果を名前変更全体やpane名変更の成功とは扱いません。

`791bd01` では、[Appleのsnapshot API](https://developer.apple.com/documentation/xcuiautomation/xcuielementsnapshotproviding/snapshot())
で値とplaceholderを同じ観測から読む方式と、即時の完全一致判定・上限付きpollを試しました。
空文字・入力後の完全一致条件、再入力回数、suiteの上限は維持します。
さらに同実行では、認証方式の切り替えから認証情報の保存設定まで約290.6秒かかっていました。
namesではこの独立した確認を外し、実際の公開鍵入力・接続・ホスト鍵確認を経て名前操作へ進めます。
fullでは認証方式の切り替え・保存設定・保存後の値確認を従来どおり実行します。
[791bd01のnames実行](https://github.com/phni3j9a/meeterm/actions/runs/34559444838)は、
一般CI・Swift/アプリビルドが成功した後、最初のHost欄で`initial_value_unavailable`となりました。
新しいsnapshot経由では値を取得できず、namesの接続・名前操作には未到達です。
xcodebuildは254秒でexit 65でした。Mainは秘密入力前の接続フォーム画像を実見しました。
snapshot取得のthrowと値のnilはこの診断だけでは区別できません。

次の修正では値の取得を実動実績のある`field.value`／placeholderの読み取りへ戻し、
即時の完全一致判定・上限付きpollとnamesのscope短縮を維持します。
新しいAPIの採用だけで改善を主張せず、実際のfixture画面での確認を必要とします。
この修正のHosted検証はまだ未完了です。

`e6cd2fe` の[names実行](https://github.com/phni3j9a/meeterm/actions/runs/34561305446)では
Host・Port・Usernameの値確認、公開鍵方式の表示、鍵入力、接続フォーム終了まで通過しました。
その後アプリが`connection_failed`となり、ホスト鍵確認は出ませんでした（exit65、約530秒）。
Mainが実見した失敗画面の接続先名は「12」でしたが、入力途中の値は期待値に一致していました。
Hostの変化か名前欄への意図しない入力かは、この記録だけでは区別できません。
names操作には未到達で、名前欄の削除待ちの修正効果も未検証です。

次の修正では即時pollを問題が観測された空欄確認に限定し、非空値の待機は従来方式へ戻します。
またConnect直前の公開フィールドを完全一致で検証し、不一致を接続失敗の前に検出します。
秘密欄は読み返さず、診断は公開フィールドの一致フラグだけを記録します。

`ae54875` の[names実行](https://github.com/phni3j9a/meeterm/actions/runs/34564459422)は、
送信前のHost・Port・Username・空のprofile名がすべて一致しました。ホスト鍵のfingerprint照合と
承認も通過しましたが、その後認証失敗となりConnectedへ進みませんでした。Mainは
秘密フォーム終了後の認証エラー画像を実見しました。名前操作には未到達です。

この段階では入力方法や認証処理をさらに変えず、失敗後の診断を追加します。
Simulatorに保存されたprofile metadataをメモリ上で期待値と比較し、同じfixture鍵を使う
通常のSSH認証も短い上限内で確認します。診断成功でUI失敗を合格へ変えることはありません。
`ios-ui-connection-diagnostics.txt` に保存情報の一致フラグとSSH probeの結果を残します。
metadata本文、秘密鍵、raw SSH出力はartifactへ保存しません。
ローカルの実OpenSSH fixtureで追加したSSH probeが成功し、SSHドライバ回帰127件も成功しました。
Simulator内の保存情報取得と、失敗時の診断artifact生成は次のHosted実行で確認します。

`bb64bae` の[fresh names実行](https://github.com/phni3j9a/meeterm/actions/runs/34568232442)は成功しました。
実SSH接続とworkspace/paneの作成・名前変更・確認付き終了を完走し、`names_complete`、
新しい `case=names result=passed`、xcodebuild終了コード0を確認しました（471.4秒）。
workspace/paneの両方で表示済みSelect Allを使い、削除後の空欄確認も通過しました。
Mainは秘密入力前の接続フォーム、host trust、workspace一覧、名前入力フォーム、作成paneの5画像を実見しました。
一般CIも両実行とも全項目成功しています。この実行では認証失敗が再現せず、失敗専用診断は
起動していません。認証不安定の原因解明やHosted metadata診断の実証とは扱いません。
次に、ビルド再利用を指定せずfresh CNGから両OSのfull受入を行います。
