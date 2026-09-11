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

端末の文字キー待機に失敗した場合は `ios-ui-terminal-keyboard-diagnostics.txt` で、
terminal・keyboard・対象キーの存在と操作可能状態を比較します。安全な画面と確認できた場合は
`terminal-keyboard-failure.png` も残します。失敗後の状態であるため、tapした瞬間の
フォーカスや入力イベントが処理されたことまでは断定しません。

自動操作では次を標準にします。

- 安定したaccessibility IDで対象を特定し、表示・操作可能状態を確認する。
- OSの初回案内が操作を覆う場合は、固有の案内文と対象ボタンを確認して一度閉じ、消失を待つ。汎用のContinueを無条件に押さない。端末のslide-to-type案内は実入力前に処理し、通常の文字キー・Paste・Returnとremote markerの確認を維持する。
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

## 検証記録

[導入時の結果と失敗調査の履歴](evidence/testing-method-validation-history.md) に、
forms/nativeの分割・ビルド再利用の実証、入力操作の修正、各runの証拠を保存しています。
最新の全体受入状況は [DAILY_USE.md](DAILY_USE.md)、評価APKは [FIRST_APP.md](FIRST_APP.md) を参照してください。
