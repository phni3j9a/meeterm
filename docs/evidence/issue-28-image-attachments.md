# Issue #28: 画像添付 受入記録

スマートフォンで選んだ1枚の画像を、選択中の SSH Server / Session / Terminal 上で既に開いている
Codex または Claude Code の会話へ渡す機能の受入記録です（PR #43）。検証は source ごとに次のとおりです。

- `8fb2a584a84c51c8fc587d51f1e681023d15a020`: 両 OS の最終 suite（Android full、iOS standard + ssh）、
  Issue #28 の実 picker と fixture E2E、本番 Rust core による実 CLI 4 組。下の表の結果はこの source のものです。
- `18141417916d54d2139cd1a82d73b419fc12a8c8`: その後の最終確認で見つかった 2 件の修正を含みます。
  Retry 受理後の転送中 Cancel（`App.tsx`）と、Herdr 遅延 test を CI の環境変数に依存させない修正（`tests/herdr.rs`）です。
  この source で、両 OS の focused 確認（build、Android Gradle unit 66/66、iOS 注入 XCTest 38/38、launch と first frame、
  attachment 11 route、fixture E2E、Retry → 転送中 Cancel の実操作）と、GitHub Actions の全 job を実施し、すべて pass しました。
  evidence は `evidence/android-20260926@4a372a3`、`evidence/ios-20260926@01cdac6` です。
- さらにその後、Cancel 後の cleanup 待ち状態を App が追跡し続ける修正（`App.tsx` の poll 条件のみ）を加えました。
  最終 head での focused 確認と CI の結果は PR #43 の本文に記録します。
- Rust の production source（`native/meeterm-core/src`）は `8fb2a58` 以降変わっていません。実 CLI の結果はその範囲の
  証拠で、アプリ全体の実機検証ではありません。

## 実装した境界

- 画像選択: Android は Photo Picker（`PickVisualMedia`）と SAF、iOS は `PHPickerViewController` と
  document picker。1操作1枚。選択されていない写真や clipboard は読みません。
- 正規化: PNG/JPEG を magic bytes で判定し、HEIC などは非対応理由を表示します。入力は stream copy 中に
  24 MiB、header から辺 16,384 px / 100 Mpx を超えるものを full decode 前に拒否します。EXIF の向き（鏡像を含む）を
  画素へ適用し、EXIF/GPS を除いて再 encode します。出力は辺 4,096 px / 16.7 Mpx / 16 MiB が上限で、
  長辺が 4,096 px を超える画像は種類に関係なく（スクリーンショットでも）縮小します。1290×2796 のような
  一般的なスマートフォンのスクリーンショットは上限内なので縮小しません。縮小後の文字の読みやすさは、
  最終確認に使った4枚（最大 2600×900 / 1290×2796）以外では確認していません。preview は実際に転送する正規化後の
  file から表示します。
- 転送: 既存の認証済み・host-key 検証済み SSH 接続上に SFTP channel を追加します（russh-sftp 3.0.0、
  russh 0.63.2）。新しい認証接続、shell/base64 fallback はありません。channel の setup も interactive
  loop を止めません。
- 保存: 既定は `~/.local/share/meeterm/attachments`（SFTP `realpath(".")` 基点。`meeterm/` と
  `attachments/` は 0700）。明示指定 directory（絶対 path または `~/`）も使えます。file 名は
  `meeterm-<YYYYMMDD>-<HHMMSS>-<16hex>.<png|jpg>` の生成名、0600、排他作成、
  `.meeterm-partial-*` へ書いて CLOSE 成功と lstat を確認してから、上書きしない SFTP v3 rename で公開します。
  path の各 component は lstat で symlink を拒否します。
- 削除: 自動削除・TTL はありません。アプリの「Delete from server」が、その操作で作成した file だけを、
  記録した canonical base と全 component を再検証してから削除します。手動削除は
  [SSH.md](../SSH.md) の手順を参照してください。
- 宛先: 添付開始時に対象 pane の native TerminalId から宛先 intent（SSH endpoint、backend と runtime
  identity。tmux は session id + server pid + start time、Herdr は stable `terminal_id`。remote pane も含む）を
  記録します。Upload・Insert・Delete のたびに現在の状態と照合し、別 Server / Session / Workspace / Terminal、
  pane の消失・置換、tmux server の置換では保留します。現在選択中の pane へ付け替えません。
- 挿入: Upload と Insert は別の明示操作です。Insert は対象 terminal 画面の toolbar から行い、core が
  remote file を再検証してから `'<remote 絶対 path>'` の単一行を既存の native paste 経路へ渡します。
  Enter や自動送信、自動再送はしません。Inserted は native 入力 queue が受理したことだけを意味し、
  CLI の読込や AI への送信成功は表示しません。IME の変換中は添付を始めず、composition を保持します。
- 状態: 1操作につき実行中の job は1つです（Cancel だけ例外）。すべての結果を attempt で照合し、
  取消後や古い job の結果は反映しません。

## 検証結果（exact source `8fb2a58`）

| 範囲 | 結果 |
| --- | --- |
| Rust core | `cargo test` 233 pass（lib）、clippy/fmt clean。fixture 付き実 OpenSSH の attachment 3本（upload/insert/stale/symlink/権限/cancel/転送中の応答性、SFTP 不可、SFTP 起動遅延の timeout）と、実 Herdr 0.9.0 の attachment 2本（upload/insert/fence/read-only 保留/controller conflict/削除、CHANNEL_SUCCESS 遅延中の入力応答）が pass |
| App / scripts | `npm run typecheck` rc=0、`npm run test:app` 102 pass、`pytest scripts/ci` 46 pass / 1 skip（JNI/C ABI の照合 test を含む） |
| Android full | `evidence/android-20260926@96d0014`。fresh CNG、`assembleRelease`、Gradle unit 66/66、screen fixtures 31/32（`empty` は既存 baseline）、attachment routes 11/11、SSH/tmux と transport-loss の契約 PASS |
| Android Issue #28 | 実 Photo Picker と SAF で正規化 preview、orientation=6 の JPEG が回転後に EXIF/GPS なし（adb で取り出して確認）、fixture sshd（SFTP）+ tmux で Upload → toolbar Insert → 入力行に引用 path が未送信で残る → Delete で remote file が消える、テスト用 IME で変換中に Attach を押すと保留され composition が残る |
| iOS standard | `evidence/ios-20260926@06e4db6`。fresh build、注入 XCTest 38/38（AttachmentTests 20/20）、26 画面と attachment 11 route、native readiness、Metal first frame、no crash |
| iOS ssh | 同じ product の reuse run で PASS（fixture topology、同一 server の操作、別 endpoint と host key の確認、transport-loss の契約） |
| iOS Issue #28 | 実 PHPicker と document picker で正規化 preview、fixture sshd（SFTP）+ tmux で Upload → Insert → 引用 path が未送信 → Delete |
| 実 CLI | [最終確認](issue-28-cli-image-final.md): 本番 Rust core を使う driver で、tmux/Herdr × Codex 0.157.1/Claude Code 2.1.283 の4組すべてで、既存会話への未送信チップ → 明示 Enter → 画像を実際に読んだ応答 → 削除まで PASS。事前の成立性確認は [probe](issue-28-cli-image-probe.md) |

## 未検証・制限

- iOS の IME 変換中の保留は、XCUITest で未確定入力を作れないため Simulator では実操作できていません。
  実 `TerminalInputView` の unit test と Android の実操作で確認しています。
- iCloud / Google Photos などの cloud provider からの原本取得、実機（physical device）、実機 GPU や IME の差は未検証です。
- remote の容量不足は sudo なしで再現できず、unit test と `statvfs@openssh.com` の事前判定だけです。
- 実 CLI の確認は、モバイルアプリではなく同じ Rust core を直接使う driver です。アプリから core までの経路は
  emulator/simulator の fixture E2E で確認しています。
- Codex 0.157.1 は起動時に直前の会話を自動で再開するため、Herdr × Codex の組は tmux × Codex と同じテスト会話を
  継続しました。
