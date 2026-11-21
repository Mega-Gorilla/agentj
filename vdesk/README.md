# vdesk — Stream Deck で Windows 11 の仮想デスクトップを切り替える OpenDeck プラグイン

## キー

| アクション | 動き |
|---|---|
| Desktop | 並べた順（上の段から左→右）に 1番目・2番目…のデスクトップを受け持つ。名前を表示し、今いるデスクトップは青＋下線。デスクトップが無い枠は消灯。押すとそのデスクトップへ |
| Desktop Info | `現在/総数` と現在の名前。押すと再読み込み |
| Previous / Next Desktop | 左隣・右隣へ。行き先の名前を表示、端では暗くなる |

デスクトップの追加・削除・名前変更・切り替えは自動でキーに反映されます（winvd の変更通知と、3秒ごとの再読み込み）。

`layout/Default.json` の配置（3×5）:

```
[1] [2] [3] [4] [5]
[6] [7] [8] [9] [10]
[◀] [n/N] [▶] [ ] [AI]
```

## AI プロファイル（Claude Code / Codex の状態）

Default プロファイル右下の「AI」キーで切り替え（AI 側の右下で戻る）。

| アクション | 動き |
|---|---|
| AI Session | 動いているセッションを1キーずつ。左上にツール名、右上にデスクトップ番号、中央にセッション名、下に状態と経過時間。押すとそのデスクトップへ |
| AI Summary | 要対応（承認待ち・入力待ち・エラー）／返答済／作業中の数。押すと一番待たせているセッションのデスクトップへ |

状態の色: 承認待ち=橙、入力待ち=黄土、返答済み=青、作業中=緑、待機中=紺、エラー=赤、15分音沙汰なしの作業中=灰「応答なし?」

- 状態はフックから: `tools/install-hooks.ps1` が `~/.claude/settings.json` と `~/.codex/hooks.json` に
  `hook-bin/agent-hook.exe` を非同期フックとして追記（既存のフックは残す、元ファイルは `.bak-日時`）。
  外すときは `pwsh tools/install-hooks.ps1 -Uninstall`
- agent-hook.exe は必要な項目だけを 127.0.0.1:47811 のプラグインへ送り、常に何も出力せず終了コード 0
- セッション名: Claude は会話記録の `custom-title`、Codex は `~/.codex/session_index.jsonl`
  （Codex の自動承認レビュー用スレッド「Guardian review」は表示しない）
- デスクトップ: Claude はセッション名と同じタイトルの Claude ウィンドウの位置。どちらも、プロンプト送信時に
  Claude / ChatGPT / ターミナルが最前面ならそのデスクトップを記録
- Codex アプリはフックを「Settings > Hooks」で承認するまで動かさない

## 仕組み

- Rust 製の単体 exe。OpenAction（Stream Deck SDK 互換）の Rust ライブラリ `openaction` で OpenDeck とつながる
- 切り替えは `winvd`（Windows の非公開 COM。24H2 26100.2605 以降、25H2 で動作確認済みのライブラリ）
- 直接切り替えに失敗したときは Win+Ctrl+←/→ を送って1つずつ移動する
- キー画像は SVG を data URL で送る（`src/render.rs`）

## 開発の流れ

```powershell
cargo build --release
.\target\release\vdesk.exe --selftest   # 読み取りだけ確認。preview.html にキーの見た目を書き出す
pwsh tools\outside.ps1 tools\deploy.ps1 # OpenDeck を止めて exe を差し替え、再起動
pwsh tools\outside.ps1 tools\deploy.ps1 -ApplyLayout   # layout\ の全プロファイル（Default / AI）も上書き（旧ファイルは .bak-日時 で残る）
pwsh tools\make-layouts.ps1             # layout\*.json を作り直す
pwsh tools\install-hooks.ps1            # Claude Code / Codex にフックを追加（-Uninstall で削除）
```

ログ: `plugin\local.virtualdesktop.sdPlugin\vdesk.log`（プラグイン）、`%LOCALAPPDATA%\opendeck\logs\OpenDeck.log`（OpenDeck）

### Claude デスクトップアプリから作業するときの注意

Claude アプリは MSIX パッケージなので、アプリ内から起動したプロセスが `%APPDATA%` / `%LOCALAPPDATA%` に書くと
`%LOCALAPPDATA%\Packages\Claude_*\LocalCache\` の私用コピーに振り替えられ、OpenDeck からは見えない。

- ソースとプラグイン本体はこの D: のフォルダに置き、OpenDeck の `plugins` フォルダからはジャンクションでつなぐ
- OpenDeck の再起動や `%APPDATA%\opendeck` への書き込みは `tools\outside.ps1` 経由（WMI で起動するとパッケージの外で動く）
- OpenDeck をアプリ内から直接起動しない（空の設定で別の OpenDeck が立ち上がる）
