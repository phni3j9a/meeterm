# Issue #1 Android 実機検証記録

この記録は Issue #1 の Android native terminal vertical slice について、「どのcodeを、どの端末で、何によって確認したか」を固定するものです。画像・録画はsource fixtureではなく検証ホスト上のrun artifactです。リポジトリへbinaryを追加していないため、下記checksumはartifactの同一性確認には使えますが、単独で画像内容を再現するものではありません。

## Final code candidate

| Item | Value |
| --- | --- |
| Code commit | `2eb050cc5ddb4667ec3e4c4d3e418906a668b0fc` |
| Branch | `feat/issue-1-android-terminal-poc` |
| Device | Google Pixel 3 (`blueline`) |
| Android / API | Android 11 / API 30 |
| ABI | `arm64-v8a` |
| Renderer / font | GLES 2.0 / bundled M PLUS 1 Code |
| APK SHA-256 | `3a8312c92490e33542cc3075e7cedc6a0085a8fbddbafa7726382f390952ca1c` |

Final code commitをcheckoutした状態で `expo prebuild --platform android --clean --no-install` によりCNG生成物を作り直し、module unit testとdebug APKをbuildしました。APKは同commitのtracked sourceから生成しています。

```text
TypeScript typecheck: pass
Expo Doctor: 21/21 pass
Rust fmt: pass
Rust test: 10 pass, 0 fail
Rust clippy -D warnings: pass
:meeterm-terminal:testDebugUnitTest: pass
:app:assembleDebug: pass (clean CNG output)
APK: arm64-v8a + x86_64 libmeeterm_core.so, M PLUS 1 Code, OFL.txt
```

## Final commitの実機再確認

Final code commitから生成したAPKをPixel 3へinstallし、Expo Development Buildをlocalhost Metroへ接続しました。次を同じnative process（PID 10094、terminal handle 1）で確認しました。

- Rust-owned terminalからnative snapshotを取得し、GLES surfaceへASCII、ANSI color/style、cursor、wrap、scrollback、CJK、combining textを描画した。
- portraitはinset反映後 `56x35`、Gboard表示後は `56x20` へresizeした。
- Gboard日本語QWERTYの `きょう` はorange underline付きnative preeditとして描画され、commit前のnative input countは増えていない。
- view detach/reattach後もterminal handle 1を再取得し、同じterminal stateを描画した。
- `finishComposingText()` 経路はfinal codeでnative commit 1回となり、sanitized logは `nativeCount=1 byteCount=15` だった。入力本文はlogに出していない。
- `commitText("今日")` のexactly-once動作は同commitのKotlin unit testでUTF-8 6 bytesとcommit 1件を確認し、Rust testでもnon-empty UTF-8 commit countを確認した。

Sanitized device logの主要部分:

```text
MeetermTerminalView: bound terminalId=poc-main handle=1
MeetermTerminalView: attached
MeetermTerminalView: resized columns=56 rows=37 cell=19x54
MeetermTerminalView: window insets left=0 right=0 bottom=132
MeetermRenderer: GLES surface created cell=19x54
MeetermRenderer: snapshot parsed columns=56 rows=37 cells=2063 visibleGlyphs=544 bytes=59882
MeetermTerminalView: resized columns=56 rows=35 cell=19x54
MeetermTerminalView: window insets left=0 right=0 bottom=936
MeetermTerminalView: resized columns=56 rows=20 cell=19x54
MeetermInput: IME commit accepted; nativeCount=1 byteCount=15
```

Final commit run artifacts:

| Artifact | Purpose | Bytes | SHA-256 |
| --- | --- | ---: | --- |
| `meeterm-2eb050c-portrait.png` | final commit portrait renderer | 274884 | `d99fea6e2b8771e8ec54dde9cfda8e750d6fc92944fecd95bf811a94dda7add9` |
| `meeterm-2eb050c-keyboard2.png` | final commit Gboard + `56x20` layout | 299380 | `70fec83b2ecd46b418999227f9a60848b7fd915e4d11d60975b9c7d9f67153a8` |
| `meeterm-2eb050c-ime-kyou.png` | final commit native `きょう` preedit | 306633 | `97466791cd0d76679660c6783c9ae34e9f746d6e8091ccc62342c6f20a633e5b` |

## Full device matrix run

独立レビュー前の同一branch working treeでは、上記に加えて次を同一build/install sessionで確認しました。このrunの後に入ったproduct-code変更は、`finishComposingText()`のcommit処理、key-up二重送信防止、strict UTF-8 decode、本番buildからのtest input buffer除外です。renderer、font、layout、resize contractは変更していません。下記をfinal commitそのものの証跡とは混同しません。

- `今日` をGboard候補からcommitし、`nativeCount=1 byteCount=6`。
- portrait `56x35` → landscape `106x17` → portrait `56x35`。
- `wm size 1080x1800` のcompact viewportで `56x28`、終了後に `wm size reset`。
- 15.99秒のscreen recordingでkeyboard表示、native preedit、commitを記録。

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| `meeterm-ime-final2-commit.png` | 301507 | `9a4ec8ec8d35a1bdefabdf6019e1d070f14905755cac95cb50a67f5597813b51` |
| `meeterm-final-landscape.png` | 233907 | `4be7ce65ef966fd1a330729a880e19d9ea6db0b431e8c51fe1d22c14c9700480` |
| `meeterm-final-compact-viewport.png` | 314477 | `00ecdf14dcdd34c72c17d8eda3657d5440fff27f31096981420156cd7a7ca0ec` |
| `meeterm-issue1.mp4` | 1173524 | `b5cab215c4cfbd58a6939e2df121c1a9f5c8e34436aa6b07a782a95932989e7d` |

## 復元状態と制限

検証後、端末のwindow-size overrideは解除済みです。system rotationは検証前の `accelerometer_rotation=1`、`user_rotation=0`、表示はphysical `1080x2160` に戻しています。

既知の制限は [Android PoC runbook](../POC_ANDROID.md#現時点の既知の制限) をsource of truthとします。特にemojiは単色glyph、dynamic font fallback chainとprocess-death recoveryは未実装です。
