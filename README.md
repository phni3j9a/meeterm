# meeterm

スマホとPCで同じリモート作業を続けるためのSSHクライアントです。
通常のtmuxと、明示的に選択した既存のHerdr runtimeに対応します。

Android開発版は[GitHub Releases](https://github.com/phni3j9a/meeterm/releases)の
**Assets → `meeterm.apk`** から取得できます。mainのCI成功後に自動生成します。
[インストール・更新・署名](docs/ANDROID_RELEASES.md)を参照してください。

## 使い方と構成

SSH接続・ホスト鍵確認の後、tmuxまたはHerdrのセッションを選択します。
Workspaceはtmux window / Herdr workspace、Terminalはpaneです。
スマホではpaneをタブとして開き、PCでは通常のtmux/Herdrクライアントで続きを操作します。
切断してもリモートの作業は残ります。一時的な通信断では作業画面を読み取り専用で保持し、
同じ接続先・端末を確認して復旧します。

React Nativeは画面・操作・低頻度の状態を担当し、SSH・端末状態・入力・描画は
共有Rust coreと薄いAndroid/iOS native adapterで処理します。
WebView端末やmeeterm専用サーバーは使いません。

実装済みの主な機能は、保存済み接続先と安全な資格情報保存、runtime picker、
workspace/group/pane操作、再接続、native入力・選択/copy、独立したapp/terminalテーマ、
特殊キー行からワンタップで行うSSH経由の画像添付です。詳細と制約は
[PRODUCT.md](docs/PRODUCT.md)から参照できます。

## 開発

```sh
npm ci
npm run typecheck
npm run test:app
npm start
```

native実行にはExpo Development Buildが必要です。Expo Goには対応しません。
`android/`・`ios/`はCNG生成物で、変更はtrackedなapp config・plugins・native moduleへ行います。

普段は変更箇所の確認だけを実行します。文書変更でnative buildを起動せず、
小さなUI修正に両OSの総合検証を要求しません。CIはPRとmainで変更範囲に応じてjobを選びます。
Android full / iOS standardなどは必要時に明示する診断です。

- [開発の入口](docs/DEVELOPMENT.md)
- [何を検証するか](docs/TESTING.md) — 実行範囲の唯一の基準
- [Mobile診断の実行手順](docs/CI_MOBILE.md) — 実行するときだけ参照
- [製品仕様](docs/PRODUCT.md) / [アーキテクチャ](docs/ARCHITECTURE.md)
- [判断原則](docs/ENGINEERING_PRINCIPLES.md)
- [SSH](docs/SSH.md) / [Herdr](docs/HERDR.md) / [UI](docs/UI_UX.md)

過去の検証結果と既知の限界は[証跡](docs/evidence/)に保存しています。
Simulatorやseeded画面の成功を、実機GPU・日本語IME・実接続の成功とは扱いません。
日常利用の経緯は[DAILY_USE.md](docs/DAILY_USE.md)、過去の評価は
[FIRST_APP.md](docs/FIRST_APP.md)を参照してください。
