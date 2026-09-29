# Android 開発版 APK の配布

[GitHub Releases](https://github.com/phni3j9a/meeterm/releases) を開き、対象の
Android 開発版の **Assets → `meeterm.apk`** をダウンロードします。
`Source code` のZIPはインストール用ではありません。APKはarm64の実機と
x86_64に対応し、JavaScript・Rustライブラリを同梱するためMetroは不要です。

## 自動生成の条件

`main` へのpush（PRのマージ・直接commitを含む）で既存の `CI` が成功すると、
`Android APK release` がそのCIの正確なコミットからビルドします。
PR・他ブランチ・失敗したCIからは配布しません。後続のマージで進行中のビルドを
キャンセルせず、コミットごとに `android-<versionCode>-<SHA>` のPre-releaseを残します。
Releases一覧でコミットを確認してください。Pre-releaseなので `/releases/latest` は使いません。

APK、`SHA256SUMS`、`build.json`（ソースSHA・versionCode・ABI・署名指紋）を添付します。
Actionsの一時artifactは7日で削除されますが、Releasesのファイルは保持します。
失敗時はActionsのログを確認して原因を修正し、該当runを再実行します。
同じコミットの再実行は同じタグへ再掲載します。

## 上書き更新と署名

既存のExpo評価APKと同じ開発用署名を使用します。証明書SHA-256は
`fac61745dc0903786fb9ede62a962b399f7348f0bb6f899b8332667591033b9c` です。
生成時に署名・package ID・versionCode・Release属性・bundle・両ABIの同梱を検査します。
これは公開されている開発用鍵で、ストア向けの秘密の配布鍵ではありません。
正式配布への切替では別途署名と既存インストールからの移行を設計します。

versionCodeは `100000 + git rev-list --first-parent --count HEAD` です。
mainを通常どおり前進させると増加し、CI再実行では変わりません。
mainの履歴を書き換えたり、この計算のoffsetを下げたりしないでください。
新しいAPKは同じpackage ID・署名の既存版へ上書き更新できます。
署名が異なる版や新しいversionCodeの版が端末にある場合は更新できません。
その場合もデータを失わないよう、安易にアンインストールしないでください。

## 検証の範囲

この配布経路は変更範囲に応じたCI・Android Releaseビルド・native単体試験・APK構造と署名の確認です。
Devin CloudのAndroid/iOS操作受入、実機の動作・性能検証とは区別します。
追加の動作確認は[TESTING.md](TESTING.md)で選び、毎回の両OS総合検証は要求しません。
mainでは一般CIのAndroid debug buildを省き、このRelease buildで確認します。
main更新ごとのAPK提供は維持するため、文書だけのmain更新でも配布ビルドは実行します。

初回配布設定の経緯・結果は[Issue #49](https://github.com/phni3j9a/meeterm/issues/49)に記録しています。
