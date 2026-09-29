# テストの選び方

この文書が検証範囲・実行タイミングの唯一の基準です。2026-09-30の軽量化方針
（Issue #50）により、以前の「UI変更でも両OS full/standard必須」や「毎回全画面を
取得する」運用を置き換えます。過去の証跡の合否は変更しません。

## 通常の変更

**変更した境界を、最も小さい有効な確認で検証します。** 全suiteを先に実行したり、
成功済みの無関係な検証を繰り返したりしません。

| 変更 | マージ前の確認 | Simulator・実機 |
| --- | --- | --- |
| 文書・証跡 | 内容・リンク・diffの確認 | 不要 |
| JSの状態・表示条件・文言 | TypeScript、App回帰 | nativeに依存しない小修正は不要 |
| レイアウト・テーマ・navigation | 上記＋変更した画面/操作の確認 | まず対象画面を1環境で確認。OS固有の差を変えた場合は該当OSも |
| Rustのparser・端末・SSH処理 | fmt/test/clippy、実OpenSSH/tmux・Herdr integration | Rustだけで再現/検証できる変更なら不要 |
| native bridge・renderer・IME・lifecycle | 関連単体/ABI/型検査、対象native build | 影響するOSで起動・native readiness・first frame・no-crashと変更操作。全画面巡回は不要 |
| 認証・接続先選択・入力・復旧のapp/native連携 | 関連App/native/実SSH回帰 | 影響するOSの実接続経路を確認。表示fixtureで代用しない |
| 依存・CNG・toolchain | lockfile/設定と影響するnative build | native ABI・起動・描画・入力へ影響する変更は対象OSで実行。共有基盤の更新なら両OS |
| CI・検証スクリプト | 変更したdriverの回帰、workflow構文/経路 | runner固有処理を変えた場合だけ対象経路を実行 |

両OSの全操作・全画面マトリクスは、広範な変更、配布前の総合確認、原因調査などで
**明示して選ぶ診断**です。通常のPRや開発APKの自動配布を一律に止める条件にはしません。
変更した動作を既存の大きなsuiteでしか確認できない場合は、そのsuiteを選ぶか、具体的な
手動確認を記録します。未実装の「短いcore smoke」を実行したことにしません。

セキュリティや破壊操作の回帰は維持します。ホスト鍵、認証、正しい接続先、入力gate、
sessionをまたぐcloseの境界を変更したときは、それを検出する試験を必ず含めます。

## 自動CI

[ci.yml](../.github/workflows/ci.yml) はPR、mainへのpush、手動dispatchで実行します。
作業ブランチのpushによる二重実行とevidenceブランチのCIはありません。
PRを更新すると同じPRの古いrunを取り消し、mainのrunはAPK配布のため維持します。

[select-checks.py](../scripts/ci/select-checks.py) が完全なGit差分からjobを選びます。
PRはmerge-baseとの差分、mainはpush前後の差分です。削除・rename元も判定し、未知の
設定ファイルは全確認へ倒します。判定失敗はCI失敗であり、無変更扱いにしません。

- 文書だけ: 選択処理だけ。アプリ/Android/iOS/Rustのjobはskip。
- App/JSだけ: TypeScript、App回帰、表示fixture契約、Expo config。
- Rust: Rust単体・実OpenSSH・SHA検証済みHerdr 0.9.0/0.9.1、関連ABI/driver回帰。
- iOS: 該当Swift preflight。アプリ全体のbuildやSimulator実行を意味しません。
- Android/native/dependency: PRでCNG/debug buildとnative単体。mainでは後続の
  [APK配布](ANDROID_RELEASES.md)のRelease build/native単体に集約します。
  main更新ごとの配布は維持するため、文書だけのmain更新でもRelease buildは実行します。
- 検証script: Python/成果物処理の回帰。Simulatorは自動起動しません。
- `workflow_dispatch`: 全job。接続テストを省略せず、必要時に手動で一式確認できます。

Expo doctorは依存変更時/手動全確認時の**参考情報**です。外部registryの推奨patchが
変わっただけで小修正に依存更新を混ぜません。`npm ci`、型検査、Expo設定、native buildの
実際の失敗は引き続きブロックします。依存更新は目的を明記して扱います。

## ローカルで使うコマンド

関連するものだけ選びます。App回帰は実際のAppをReactで動かし、native bridgeをstubに
するため、端末描画やSSH接続の証明ではありません。

```sh
npm run typecheck
npm run test:app
node --test --test-name-pattern='対象の名前' scripts/ci/app-selection.test.cjs
cargo fmt --check --manifest-path native/meeterm-core/Cargo.toml
cargo test --locked --manifest-path native/meeterm-core/Cargo.toml
cargo clippy --locked --manifest-path native/meeterm-core/Cargo.toml --all-targets -- -D warnings
python3 -m unittest discover -s scripts/ci -p 'test_select_checks.py'
python3 -m unittest discover -s scripts/ssh -p 'test_fixture.py'
git diff --check
```

実SSHは `python3 scripts/ssh/fixture.py -- <cargo test ...>` で隔離環境を使います。
具体的なtest名とHerdr取得・SHA検証はci.yml、接続契約は[SSH.md](SSH.md)と
[HERDR.md](HERDR.md)を参照します。ユーザーのsessionやインストール済みHerdrを変更しません。

## Mobile診断と報告

実行するときだけ[CI_MOBILE.md](CI_MOBILE.md)を読み、対象commit・OS・suite・確認目的を
指定します。Devin CloudはSWE-2を使用し、別モデルへ黙って変更しません。

実行したsuiteのassertionや失敗条件は維持します。最初の失敗を調査し、blind rerunや
期限延長で合格にしません。画像を実際に開くまで、その画像の視覚的成功は主張しません。
一方のOSを見た結果を両OSの成功にしません。seeded画面は接続・保存・入力の証明になりません。
Metal/CoreGraphics、Simulator/実機、日本語IMEの検証範囲も分けます。

通常はPRに「結果・実行した確認・残事項」を短く記載します。Mobileを実行した場合だけ、
source SHA・suite・fresh build/再利用元・成果物リンク・失敗/未確認事項を添えます。
毎回新しいevidence文書を作ったり、同じ結果をREADME・仕様書・手順書へ重複転記したり
する必要はありません。過去の詳細は[検証履歴](evidence/testing-method-validation-history.md)。
