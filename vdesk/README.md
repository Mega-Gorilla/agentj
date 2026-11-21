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
[◀] [n/N] [▶] [ ] [ ]
```

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
pwsh tools\outside.ps1 tools\deploy.ps1 -ApplyLayout   # キー配置も layout\Default.json で上書き（旧ファイルは .bak-日時 で残る）
```

ログ: `plugin\local.virtualdesktop.sdPlugin\vdesk.log`（プラグイン）、`%LOCALAPPDATA%\opendeck\logs\OpenDeck.log`（OpenDeck）

### Claude デスクトップアプリから作業するときの注意

Claude アプリは MSIX パッケージなので、アプリ内から起動したプロセスが `%APPDATA%` / `%LOCALAPPDATA%` に書くと
`%LOCALAPPDATA%\Packages\Claude_*\LocalCache\` の私用コピーに振り替えられ、OpenDeck からは見えない。

- ソースとプラグイン本体はこの D: のフォルダに置き、OpenDeck の `plugins` フォルダからはジャンクションでつなぐ
- OpenDeck の再起動や `%APPDATA%\opendeck` への書き込みは `tools\outside.ps1` 経由（WMI で起動するとパッケージの外で動く）
- OpenDeck をアプリ内から直接起動しない（空の設定で別の OpenDeck が立ち上がる）
