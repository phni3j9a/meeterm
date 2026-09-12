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
