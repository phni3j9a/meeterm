# テスト方法の導入・調査記録

以下はテスト方法を整備した際の時系列の記録です。「次の実行」「未完了」などは
各時点の状況を表します。通常の実行手順は [標準テスト手順](../TESTING.md)、
最新の機能受入状況は [日常利用の検証記録](../DAILY_USE.md) を参照してください。

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

`516380f` の[fresh両OS/full](https://github.com/phni3j9a/meeterm/actions/runs/34570866987)では、
Androidは69記録と正常終了を確認し、Mainが4画像と評価APKを確認しました。iOSはbuild・保存4件・
入力7件を通過後、host trustの承認後に認証失敗となりました（UI XCTest exit65、464.9秒）。
新しい接続診断が実際に取得され、保存metadataはusernameだけがfixture期待値と不一致、
他フィールドは一致、strict SSH probeは成功でした。送信前の画面側Usernameは一致しています。
Mainは認証エラー画像と、期待するユーザー名が見える秘密入力前のpasswordフォームを実見しました。

この証拠から、次はUsernameだけ初回から既存の1文字入力・prefix確認を使います。
一括のsynthetic入力とReact state反映のずれが有力ですが、event欠落の機序は断定しません。
prefix確認もnative/AX値なので、React stateそのものの証明とは扱いません。秘密欄・product・
他の短い入力欄・retry数・制限時間は変更しません。Swift型チェック後にfresh iOS/fullを実行します。
今回の差分はiOSテストドライバだけで、Androidのアプリ/native/workflowは`516380f`から不変のため、
成功済みAndroid fullは再実行しません。最終証拠はOS別のcommit/runとして記録します。

`b94ef2b` の[fresh iOS/full](https://github.com/phni3j9a/meeterm/actions/runs/34574408722)では、
保存metadataがUsernameを含め全て一致し、strict SSH probeも成功しました。認証・実端末入力・
切断と再接続・同一paneの復元まで進み、再接続後の最初の文字キー `t` の存在確認で失敗しました
（UI XCTest exit65、1000.7秒）。保存4件・native入力7件は成功しましたが、daily完了と最後の
fresh foundationには未到達です。Mainは初回keyboard・入力・切断後の3画像を実見しました。
失敗した瞬間の画像がなく、keyboard非表示とlayout/AX queryの違いはまだ区別できません。

この境界には、既存10秒待機の失敗時だけ `ios-ui-terminal-keyboard-diagnostics.txt` を追加します。
foreground、フォーム消失、terminal/keyboard/Paste/Hide keyboard/要求キー/大文字キー/同labelの
buttonの存在・hittableを固定フラグで保存し、入力値やコマンドは出力しません。
foregroundかつ接続フォーム消失かつterminal存在のときだけ `terminal-keyboard-failure.png` を
取得し、元の失敗を維持します。自動再tap・入力迂回・layout切替・制限時間延長は行いません。
診断ファイルと画像は開始時に削除し、古い結果を混在させません。次のfresh iOS/fullで確認します。

### キーボード初回案内の観測

`ce75d8e` の [fresh iOS/full](https://github.com/phni3j9a/meeterm/actions/runs/34578966905)
はSwift・build・保存4件・native入力7件・SSH認証に成功しました。保存metadataの全一致と
strict SSH probe成功も再確認できました。初回の文字キーqueryを通過後、Pasteのhittable待機で
失敗しました（source1628、UI317.4秒、xcodebuild exit65・449.2秒）。dailyと最終foundationは
未到達です。文字キー待機は失敗していないため、新しいfailure-only keyboard診断は生成されませんでした。

Mainが `terminal-keyboard.png` を実見したところ、iOSのslide-to-type初回案内とContinueが
キーボード領域を覆っていました。文字キー/PasteがAX上に存在するだけでは、案内に妨げられず
実際に入力できることの証明になりません。次は固有の案内文とContinueを確認して一度閉じ、
消失確認後に従来の文字キー・native Paste・Return・remote marker検証を行うdriver修正を追加しました。
全5回の共通コマンド入力の冒頭で確認し、案内不在なら無操作です。各操作待機は10秒、
再試行・キーボード設定変更・全体時間上限の変更はありません。Hosted検証は未実施です。
`b94ef2b` では失敗時の画像がなく、同じ案内が原因だったとは断定しません。
