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
| Android | 既存のfull smokeと画像の実見。Herdr 4画面はfresh processのoptional observational fixture | Androidの自動操作とnative境界。fixtureは表示確認でmachine gateではない |
| iOS `standard` | production保存4件、native入力7件、14画面の撮影、native起動・readiness・first frame・no-crash | iOSの保存/入力実装、画面表示、実native端末描画 |
| iOS `ssh` | 接続、ホスト鍵確認、短い端末入力、リモート側の到達確認、切断 | iOSの実SSHとnative端末入力の接続境界 |

`standard`をiOSの既定suiteにします。`ssh`は接続・認証・入力・native連携に影響する変更と配布前に実行します。
今回の方針導入時は、fresh CNGでAndroid fullとiOS standardを確認し、同一ソースのiOS sshも確認します。
`full`の全操作成功は、この新しい通常検証や日常利用マイルストーンの必須条件ではありません。

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
python3 -m unittest discover -s scripts/ssh -p 'test_*.py'
python3 -m unittest discover -s scripts/ci -p 'test_*.py'
git diff --check
```

全コマンドを毎回実行する必要はありません。変更に関連するチェックを選びます。
macOSでは `scripts/ci/ios-typecheck.sh` がCNG/build前にUI XCTestとnative入力関連Swiftを型チェックします。
production moduleへ依存する保存テストのコンパイル・Keychain実行はアプリビルドとnativeテストで確認します。

## 画面の撮影方法

通常のアプリでフォーム入力・接続・作成を順に実行してから撮影する方法を、表示確認の前提にしません。
smoke buildと明示したテスト起動URLを組み合わせ、固定の公開データで対象画面を直接開きます。
本番と同じ画面コンポーネントを使い、撮影用の画面を別実装しません。

対象はホーム、保存済みサーバー、鍵認証フォーム、パスワード認証フォーム、
ワークスペース一覧、ターミナル、設定、ワークスペース名、ターミナル名、PC引き継ぎに加え、
Herdr connection、groups、terminal、workspaces の14画面です。
`meeterm://smoke?screen=<名前>` で直接開き、`standard-<名前>.png` に保存します。
名前は順に `home`、`servers`、`connection`、`password`、`workspaces`、`terminal`、
`settings`、`workspace-name`、`terminal-name`、`handoff`、
`herdr-connection`、`herdr-groups`、`herdr-terminal`、`herdr-workspaces` です。
撮影用設定はライト表示に固定します。最後の新規起動によるnative foundationは `terminal.png` に保存します。

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
| `ssh` | 実SSH接続と短いnative入出力の確認 |
| `native` | 保存4件（legacy profileのbackend/runtime既定値を含む）とnative入力7件だけの限定確認 |
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
OSの初回案内は固有の文章を確認して一度閉じ、消失後に通常操作を行います。
端末のキー待機失敗では `ios-ui-terminal-keyboard-diagnostics.txt` を確認します。
実接続失敗では保存metadataの一致フラグとstrict SSH probeを確認できますが、事後probe成功だけでUI入力成功は証明できません。
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
