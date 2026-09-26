# Issue #28 最終実装 CLI 画像読込確認 — 4 組本実行 (AC18/AC19/AC20)

- 実施: 2026-09-27 (JST)、隔離環境 (README-rerun.md 手順)
- 対象実装: `meeterm-core` (Rust native core) の添付 API を driver が path 依存で直接実行
  - `connect_host` → host-key 検証 → runtime discovery → `select_runtime` → `select_pane`
  - `attachment_intent` → `attachment_begin` (SFTP upload) → `Uploaded`
  - `attachment_insert` → job 完了 (0x4 クリア) → `Inserted` + `INSERT_ENQUEUED_UNCONFIRMED`
  - pane 観測で**未送信**確認 → Enter を**別操作**で送信 → CLI 応答確認
  - `attachment_delete_remote` → remote 残存なし
- 統合候補 SHA: `cea0032302abc0db3d65e75f5ee51e69aa137bb8`
  - 実行時 HEAD: `8fb2a584a84c51c8fc587d51f1e681023d15a020` (App.tsx 系のみの後続)
  - `git diff cea0032 -- native/` = 空 (検証済み。driver は候補の Rust core を実行)
- 画像は毎組新規合成 (コードは filename/依頼文に含めず別メモ管理)。正解一致で実読込を判定。

## 結果サマリ — 4/4 PASS

| # | backend × CLI | image | remote path (生成名) | chip未送信 | Enter別送 | 応答コード | remote削除 |
|---|---|---|---|---|---|---|---|
| 1 | tmux × Codex | PNG 900x700 | `~/.local/share/meeterm/attachments/meeterm-<ts>-bd83e6c7….png` | `[Image #1]` | ✓ | 正解一致 | ✓ |
| 2 | tmux × Claude Code | JPEG 850x650 | `~/.local/share/meeterm/attachments/meeterm-<ts>-7ad2f42c….jpg` | `[Image #1]` | ✓ | 正解一致 | ✓ |
| 3 | Herdr × Codex | PNG 1290x2796 | `~/.local/share/meeterm/attachments/meeterm-<ts>-6a3ddc74….png` | `[Image #1]` | ✓ | 正解一致 | ✓ |
| 4 | Herdr × Claude Code | PNG 2600x900 | `~/.local/share/meeterm/attachments/meeterm-<ts>-49c7c208….png` | `[Image #1]` | ✓ | 正解一致 | ✓ |

## 環境・設定

- SSH: probe 専用 `sshd` (127.0.0.1 高位ポート, pubkey, `AcceptEnv`/`SetEnv` で `PATH`+`TMUX_TMPDIR` 隔離, `AllowTcpForwarding local` + `AllowStreamLocalForwarding yes`)
- tmux 3.4 (probe 専用 socket/session `probe`)、Herdr 0.9.0 (probe 専用 session `fp28`、隔離 config/state)
- Codex CLI 0.157.1、model `GPT-6-Sol xhigh`、permission `danger-full-access` (config)
- Claude Code 2.1.283、model `Opus 5.5`、manual permission mode
- CLI 起動は各 pane で driver が `commit_utf8`/`send_special_key` (アプリと同一入力 API) で実施
  - Codex: `codex` 起動 → folder trust 承認 → composer
  - Claude: `claude -p <premode>` で会話作成 → `claude --resume <uuid>` で同一会話を直接オープン (セッション一覧 UI は開かない)

## 各組の記録

### 1) tmux × Codex — PASS

- cwd: probe work dir (`work/final-tmux-codex`)
- 挿入形式: bare remote path の paste → composer が `[Image #1]` chip 化 (pane capture で未送信滞留を確認)
- Enter は挿入後の別操作 → transcript (rollout jsonl) に
  `{"type":"message","role":"user","content":[{"type":"input_text","text":"<image name=[Image #1] path=\".../meeterm-<ts>-bd83e6c7….png\">"},{"type":"input_image","image_url":"data:..."}]}`
  → assistant `output_text` に正解コード → **実画像ペイロードがモデルへ到達**
- 同一会話が継続 (premode の応答→chip→応答が同一 rollout 内に連続)

### 2) tmux × Claude Code — PASS

- cwd: probe work dir (`work/final-tmux-claude`)
- 挿入形式: 同上 → `[Image #1]` chip、未送信滞留 → 別 Enter
- transcript (session jsonl): user message に
  `{"type":"text","text":"[Image: source: .../meeterm-<ts>-7ad2f42c….jpg]"}` +
  `{"type":"image","source":{"type":"base64","media_type":"image/jpeg","data":...}}` (b64 約 34.8k chars)
  → assistant がコードを正答 + 画像内容(背景色・図形配置)を記述 → **添付 image ブロックとして実読込**
- cwd 外 (`$HOME` 配下) の参照でも permission prompt は出ず (manual mode・観測値)

### 3) Herdr × Codex — PASS

- pane: Herdr workspace pane (`fp28` session)、cwd = probe work dir (`work/final-herdr-codex`)
- 入力経路: `pane.send_text` / `pane.send_keys` / `pane.send_input` (SemanticInput, production の controller 経路)
- `codex` 起動時に直前会話を auto-resume (Codex 0.157.1 の既定動作)。会話は probe 作成のテスト会話で先行発話済みのため要件を満たす (注記)
- 挿入形式: `[Image #N]` chip 未送信 → 別 Enter → transcript に `input_image` + 正解コード
- wait/lease: controller は core が接続内で管理。driver は通常 API のみ使用 (takeover なし)

### 4) Herdr × Claude Code — PASS

- pane: `fp28` の Claude workspace、cwd = probe work dir (`work/final-herdr-claude`)
- `claude -p` → `--resume` で会話オープン (一覧 UI 不使用)
- 挿入形式: `[Image #1]` chip 未送信 → 別 Enter → transcript に `[Image: source: ...]` text + `image/png` base64 ブロック → 正解コード + 内容記述

## 補足所見 (実装/ドキュメントへの示唆)

- **Codex の folder trust gate は複数回出得る**: 起動時だけでなく submit 後にも `Folder access` ダイアログが再提示されるケースを観測 (accept は Enter)。driver 側で「ダイアログ検出 → Enter → composer 滞留なら再 Enter」のポンプが必要だった。アプリ側ではユーザー操作として現れる想定だが、probe 自動化では注意点。
- **`send_bytes` は Herdr binding では使えない** (Semantic transport → `InvalidKey`/`Internal`)。typed text は `commit_utf8` (`SemanticInput::Text`→`pane.send_text`) が正しい入口。添付 insert は内部で `pane.send_input` paste 経路を使用し chip 化を確認。
- **shell コマンド行への日本語 typed input は文字化けした** (`<ffffffff>` 化)。TUI composer への `commit_utf8` や paste 経路は UTF-8 正常。premode を ASCII 化して回避 (probe 側制約; 添付 path は ASCII のみなので本件には無関係)。
- **Codex 0.157.1 は bare `codex` で直前会話を auto-resume** する。Herdr pane で起動したら tmux pane 作成の会話に接続された (rollout 共有)。テスト観点では「既開会話」要件を満たすが、コンボ分離には `--resume`/`fork` 等の明示が必要。

## 後片付け

- probe sshd / probe tmux server / Herdr `fp28` server / CLI プロセス: 全停止 (port 解放確認)
- remote 添付: 全件 `attachment_delete_remote` で削除、`~/.local/share/meeterm/` 配下残存なし
- Codex trust: probe cwd の追加分を削除 (config から除去済み)
- テスト会話履歴 (rollout/session jsonl) は指示どおり保持
- リポジトリ変更なし (`git status` clean)
