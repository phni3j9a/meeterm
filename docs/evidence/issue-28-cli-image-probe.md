# Issue #28 CLI画像読み取り 実機プローブ証跡 (sanitized)

W1 先行成立性確認 (AC18/AC19/AC20)。meeterm が想定する「選択中の SSH Server/Session/Terminal 上で既に開かれた Codex / Claude Code 会話へ、1枚の画像を渡す」操作を、実際の CLI バイナリと実際の入力経路で検証した。

すべて隔離環境で実施。ユーザーの tmux サーバ・Herdr セッション・CLI 会話・設定には一切触れていない。秘密情報・アカウント情報・会話IDは含めない。

## 環境

| 項目 | 値 |
|---|---|
| Codex | codex-cli 0.157.1 (model: GPT-6-Sol xhigh, 権限: danger-full-access — 既存ユーザー設定を読み取ったのみ。変更なし) |
| Claude Code | 2.1.283 (model: Opus 5.5, manual permission mode — TUI表示値) |
| tmux | 3.4 (probe 専用 socket `tmux -S <probe>/tmux/tmux.sock`, session `probe`, panes %1/%2) |
| Herdr | 0.9.0 (protocol 22 / schema 1, CI と同一手順で取得・sha256 検証済み、専用 session `fp28`, 隔離 XDG/設定) |
| SSH | 隔離 sshd, 127.0.0.1:48731, publickey のみ, host key 検証済み (known_hosts 固定)。SFTP subsystem 有効 |
| probe root | `/tmp/frontierplan-sn50_1s8/probe` |

### 隔離 sshd の逸脱点 (記録)

- `StrictModes no` を使用: `/tmp` 配下の authorized_keys は OpenSSH の厳格モードで拒否されるため、probe 用 sshd のみで無効化。システム sshd 無関係。
- `AllowTcpForwarding` は `no` だと direct-streamlocal も拒否されるため `local`→`yes` で検証 (後述)。
- SFTP `realpath` は対話クライアントで未対応 (転送自体は正常)。

### Herdr 入力経路の前提

- コントローラリース: `terminal session control <terminal_id> --cols N --rows N` を SSH exec で保持 (stdin が EOF すると即 `detached` で閉じるため、開いたままの stdin が必須。meeterm の russh チャネルは常時オープンで整合)。
- 貼付は `pane.send_input`、Enter は `pane.send_keys` `["enter"]` — `herdr_control.rs` の SemanticInput::Paste/Key 分岐と同一。
- tmux は `send-keys -H` (bracketed-paste envelope `\x1b[200~…\x1b[201~` を生成側で付与) — `tmux.rs`/`terminal.rs` の `paste_utf8` と同一。
- いずれも Enter は貼付に含めず、別操作として送信 (製品の「自動送信しない」契約と同一)。

## テスト画像 (合成・ファイル名に答えを含まない)

| 画像 | サイズ | 形式 | 内容 (正解キー) |
|---|---|---|---|
| A | 800×600 | PNG | 黄背景+幾何図形+コード `MYU6-FPPS` |
| B | 900×700 | JPEG | 濃紺/緑の2色背景+白文字 `7YSW-VBUA` |
| C | 1290×2796 | PNG | スマホ風スクリーンショット(チャット画面)+吹き出し内 `W9EW-JWB8` |
| D | 2600×900 | PNG | 横長バナー+ストライプ+黒帯に黄文字 `LM43-XHHU` |

配置は2か所: (a) CLI の cwd 配下 `work/<backend>-<cli>/attachments/` (0755/0644), (b) アプリ既定添付先 `$HOME/.local/share/meeterm/attachments/op-<backend>-<cli>/` (0700/0600)。SFTP で転送。

## 結果マトリクス

| # | backend × CLI | 挿入フォーム | 未送信滞留 | Enter別送 | 画像実読 | 結果 |
|---|---|---|---|---|---|---|
| 1 | tmux × Codex | bare path (a)(b) / request+quoted (a) | `[Image #1]` チップで滞留 | 手動 Enter | `view_image`/`input_image` transcript + 全コード正解 | **PASS** |
| 2 | tmux × Claude | bare path (a)(b) / request+quoted (a) | `[Image #N]` チップで滞留 | 手動 Enter | `Read` tool→image content + 全コード正解 | **PASS** |
| 3 | Herdr × Codex | bare path (b) / quoted path (a) / request+quoted JA・EN (a) / bare path with spaces | `[Image #1]` チップ (space入りは plain text) | 手動 Enter | `view_image`/`input_image` transcript + 全コード正解 | **PASS** |
| 4 | Herdr × Claude | bare path (b) / quoted path (a) / request+quoted JA・EN (a) | `[Image #1]`/`[Image #2]` チップ | 手動 Enter | `Read` tool→image content + `input_image` + 全コード正解 | **PASS** |

4/4 PASS。

## 詳細証跡

### 1. tmux × Codex — PASS

- 会話事前交換: 合言葉 `FP28T1` を send-keys 経路で送り、応答確認 (同一 pane・同一セッションで継続)。
- (i) bare path `$HOME/.local/share/meeterm/attachments/op-tmux-codex/att.png` (画像A): composer に `› [Image #1]` とチップ表示・未送信。Enter 別送 → `画像内の文字は「MYU6-FPPS」` + 図形の説明 (正解)。
- (ii) `この画像を読んで内容を説明してください: '<cwd>/attachments/photo.jpg'` (画像B): plain text で滞留 → Enter → `Viewed image photo.jpg` + `7YSW-VBUA` (正解)。
- (iii) C (1290×2796) request+quoted → `W9EW-JWB8` (正解。CLI側リサイズ後も読取可)。
- (iv) D (2600×900) request+quoted → `LM43-XHHU` (正解。2048px超でも可)。
- transcript (`~/.codex/sessions/.../rollout-*.jsonl`) に `custom_tool_call` (view_image相当) と `input_image` (base64画像ペイロード) を確認。パス表示やOCRではなく実画像がモデルへ渡っている。

### 2. tmux × Claude Code — PASS

- 会話事前交換: 新規テスト会話で合言葉 `FP28T2` 確認 (ダッシュボード経由で作成→再オープン)。
- (i) bare path (画像A, cwd): `❯ [Image #1]` 未送信 → Enter → `Read 1 file` + `MYU6-FPPS` + 図形説明 (正解)。
- (ii) `$HOME` パス explicit request (画像B): `Read 1 file` + `7YSW-VBUA` (正解)。**cwd 外でも許可プロンプトなし** (manual mode の当該設定下。一般化はしない)。
- (iii) C (1290×2796) → `W9EW-JWB8` + スクリーンショット内容の詳細 (正解)。
- (iv) D (2600×900) → `LM43-XHHU` + バナー構成説明 (正解)。
- transcript に `tool_use: Read {file_path}` → `tool_result: ["image"]` を確認。

### 3. Herdr × Codex — PASS

- pane `w1:p1` (terminal_id 取得済み), cwd=`work/herdr-codex`, `pane.send_input` で貼付。
- 会話事前交換: 合言葉 `FP28H1` → 応答確認。
- (i) bare path `$HOME` (画像A): `› [Image #1]` 未送信 → Enter → `MYU6-FPPS` (正解)。
- (ii) JA request + quoted path (画像B): `Viewed image photo.jpg` + `7YSW-VBUA` (正解)。
- (iii) **quoted bare path** `'.../banner.png'` (画像D): 引用符付きでも `[Image #1]` チップ化 → `LM43-XHHU` (正解)。
- (iv) EN request + quoted path (画像C): `Viewed image att.png` + `W9EW-JWB8` (正解)。
- (v) **space を含む bare path** `.../dir with space/space img.png` (画像A): チップ化せず plain text 滞留。Enter → `Viewed image space img.png` + `MYU6-FPPS` (正解)。space入りでも読めるが attachment chip UX は失われる。
- transcript に `view_image` call + `input_image` 出力を確認。

### 4. Herdr × Claude Code — PASS

- pane `w1:p2` (別 terminal_id), cwd=`work/herdr-claude`。新規テスト会話 `FP28H2`。
- (i) bare path `$HOME` (画像A): `❯ [Image #1]` 未送信 → Enter → `Read 1 file` + `MYU6-FPPS` (正解。cwd外・許可プロンプトなし — 当該設定下の観測)。
- (ii) JA request + quoted (画像B): `Read 1 file` + `7YSW-VBUA` (正解)。
- (iii) quoted bare path (画像D): `[Image #2]` チップ化 → `LM43-XHHU` (正解)。
- (iv) EN request + quoted (画像C): `Read 1 file` + `W9EW-JWB8` + メッセージ一覧の再読取 (正解)。
- transcript に `tool_use: Read` → `tool_result: image`、およびユーザーメッセージ内の image block (chip→画像添付) を確認。

## プロンプトフォーム比較

| フォーム | Codex | Claude | 備考 |
|---|---|---|---|
| bare path (space無し) | `[Image #1]` チップ→そのまま画像入力 | `[Image #N]` チップ | 最もシンプル。ファイル名が答えを含んでも画像自身を読む |
| quoted bare path `'path'` | 同じくチップ化 | 同じくチップ化 | 引用符があってもチップ化する |
| request + quoted path | plain text→`view_image` tool | plain text→`Read` tool | 最も明示的。path が space を含んでも機能 |
| bare path (space含み) | plain text→`view_image` で読めるが chip にならない | (未個別検証) | space回避または request+quoted を推奨 |

- パス中の `'` は引用テンプレートと衝突し得る → アプリ側で添付ファイル名を `[A-Za-z0-9._-]` に正規化すべき (製品側で制御可能)。
- 入力長: テストは ~120 文字。terminal.rs の `MAX_INPUT_BYTES` (64KiB) が上限。実用上の制約なし。
- 改行・制御文字は paste_utf8 で除去/正規化される (terminal.rs)。パスに含めない。

## 画像サイズ・形式

| 条件 | 結果 |
|---|---|
| 通常 PNG 800×600 | 両CLI 読取 OK |
| JPEG 900×700 | 両CLI 読取 OK |
| 1290×2796 (phone screenshot 相当) | 両CLI 読取 OK (CLI 側で縮小しても文字コード読取可) |
| 2600×900 (>2048px 辺あり) | 両CLI 読取 OK |
| 制限値 | 観測上の拒否なし。公式ドキュメント上の上限は未測定 (要別途確認) |

## 保存先パス (`~/.local/share/meeterm/attachments`)

- 両 CLI とも cwd 外の `$HOME/.local/share/meeterm/attachments/op-*` 配下を `0700/0600` で読取可。
- Claude は本 probe の permission mode (manual) では cwd 外でも許可プロンプトを出さなかった。利用者の permission mode によっては初回の許可対話が出る可能性 → 製品 UX 上要考慮 (モード別の挙動は今回の範囲では未検証)。

## 失敗・制約

- Herdr `terminal session control` は stdin EOF で即 `detached` → リースは開いたストリームを維持する実装が必須 (meeterm の russh exec と整合)。
- OpenSSH `AllowTcpForwarding no` は direct-streamlocal forward も拒否 → Herdr 経路の SSH transport には tcp forwarding 許可が必要 (少なくとも `local`)。
- tmux/Claude のダッシュボード操作時にユーザー既存セッションを1度誤って開いた (Escapeで即離脱・変更なし)。probe 上の安全インシデントとして記録。
- 各 CLI の model/permission は実行者の環境依存 — 別環境での再現性はモデル能力・許可設定に左右される。

## 推奨挿入テンプレート (実装向け)

1. 基本: bare path paste (space・`'` を含まない正規化パス) → attachment chip → Enter別送。
2. 堅牢: `<短い要求>: '<正規化path>'` (space・特殊文字に強い)。
3. 添付ファイル名はアプリが発行 (`att_<ts>.png` 等) して space・引用符・非ASCII を排除するのが安全。
4. いずれも Enter は paste に含めず、ユーザー操作で送信 — 本 probe と同一契約で成立することを確認済み。
