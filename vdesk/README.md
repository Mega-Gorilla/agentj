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

Default プロファイル右下の「AI」キーで切り替え（AI 側の左下で戻る）。

`layout/AI.json` の配置（3×5）:

```
[Claude] [Claude] [Claude] [Claude] [Claude]
[Codex]  [Codex]  [Codex]  [Codex]  [Codex]
[戻る]   [ ]      [◀]      [集計]   [▶]
```

縦は上が Claude・下が Codex、横はデスクトップ番号の順。セッションのあるデスクトップが左から1列ずつ使い、
同じデスクトップの Claude と Codex は上下に並ぶ（同じツールが1つのデスクトップに2つあれば2列使う。
セッションの無いデスクトップは列を使わない）。5列に入りきらないときは ◀ ▶ でページを切り替える
（1つのデスクトップがページをまたがないように、収まらない列は次のページへ送る）。

AI Session キーの設定 `"tool": "claude"` / `"codex"` で受け持つ段を決め、各段の k 番目のキーが k 列目になる。
設定の無い AI Session キーには、段の無いツールのセッション（どのキーにも設定が無ければ全部）を順に並べる。

| アクション | 動き |
|---|---|
| AI Session | 動いているセッションを1キーずつ。左上にツール名、右上にデスクトップ番号（不明なら点線の「?」）、中央にセッション名、下に状態と経過時間。押すとそのデスクトップへ |
| AI Summary | 要対応（承認待ち・入力待ち・エラー）／返答済／作業中の数（全ページ分）。押すと一番待たせているセッションのデスクトップへ |
| AI Page Previous / Next | 前／次のページへ。行き先のページ番号（例 `2/3`）を表示し、その先に要対応のセッションがあれば橙。ページが1つなら暗い |

状態の色: 承認待ち=橙、入力待ち=黄土、返答済み=緑、作業中=黒（目立たせない）、自動審査中=黒に近い灰、待機中=ほぼ黒、エラー=赤、
15分音沙汰なしの作業中=灰「応答なし?」。要対応に数えるのは承認待ち・入力待ち・エラーだけ。

- 状態はフックから: `tools/install-hooks.ps1` が `~/.claude/settings.json` と `~/.codex/hooks.json` に
  `hook-bin/agent-hook.exe` を非同期フックとして追記する。足したり外したりするのは agent-hook.exe の
  ハンドラーだけで、同じグループにある他のコマンドも残す。内容が変わるときだけ書き換え、元ファイルは
  `.bak-日時`。外すときは `pwsh tools/install-hooks.ps1 -Uninstall`
- agent-hook.exe は必要な項目だけを 127.0.0.1:47811 のプラグインへ送り、常に何も出力せず終了コード 0。
  ツールの入力は送らず、そのハッシュ値だけを送る（どのツール呼び出しの承認要求かを見分けるため）
- 非同期フックは届く順番が保証されないので、状態はフック内で取った時刻で判断する。終了したセッションへ
  遅れて届いたイベントでは復活させず、再開後に届いた古い終了イベントでは消さない
- 承認待ちはツール呼び出しごとに持つ。その呼び出しが終わる（`PostToolUse`）か、ターンやバッチが終わる
  まで残り、並列の別のツールが終わっても消えない。「承認された」を知らせるフックはどちらにも無いので、
  承認後に長く動くツールは終わるまで承認待ちのままになる
- Codex の自動審査（`~/.codex/config.toml` の `approvals_reviewer = "auto_review"` / `"guardian_subagent"`）
  では、承認要求は利用者ではなく審査役に回る。フックからは見分けられないので、この設定のときは
  「自動審査中」と表示して要対応に数えない。2分たっても解決しなければ承認待ちに切り替える（アプリが
  スレッドごとに審査役を変えている場合に見落とさないため）
- セッション名: Claude は会話記録の `custom-title`（最初の1回はファイル全体、以後は増えた分だけ読む）、
  Codex は `~/.codex/session_index.jsonl`（Codex の自動承認レビュー用スレッド「Guardian review」は表示しない）
- デスクトップ: Claude はセッション名と同じタイトルの Claude ウィンドウの位置（同じタイトルのウィンドウが
  別々のデスクトップにあるときは決めつけず、記録済みのまま）。タイトルが「Claude」だけの
  ウィンドウが1つ、名前の合うウィンドウが無いセッションが1つなら、その2つを結び付ける。
  どちらも、プロンプト送信時に Claude / ChatGPT / ターミナルが最前面ならそのデスクトップを記録。ただし
  - 作業中に送ったメッセージは順番待ちになり、フックが届くのは渡されたとき（その間に別のデスクトップへ
    移っていることが多い）なので記録しない
  - 最前面が別のセッション名のウィンドウなら記録しない
  ChatGPT のウィンドウ名はどれも「ChatGPT」で見分けられないので、まだ記録が無い Codex スレッドは同じ名前の
  Claude セッションのデスクトップを使う（同名が別々のデスクトップにあるときは「?」のまま）
- Codex アプリはフックを「Settings > Hooks」で承認するまで動かさない

## 仕組み

- Rust 製の単体 exe。OpenAction（Stream Deck SDK 互換）の Rust ライブラリ `openaction` で OpenDeck とつながる
- 切り替えは `winvd`（Windows の非公開 COM。24H2 26100.2605 以降、25H2 で動作確認済みのライブラリ）
- 直接切り替えに失敗したときは Win+Ctrl+←/→ を送って1つずつ移動する
- キー画像は SVG を data URL で送る（`src/render.rs`）

## 開発の流れ

```powershell
cargo test --release                    # 並べ方・状態の決め方などの単体テスト
cargo clippy --locked --all-targets -- -D warnings
cargo build --release
.\target\release\vdesk.exe --selftest   # 読み取りだけ確認。preview.html にキーの見た目を書き出す
pwsh tools\outside.ps1 tools\deploy.ps1 # OpenDeck を止めて exe を差し替え、再起動。失敗時は終了コード 1（OpenDeck は起動し直す）
pwsh tools\outside.ps1 tools\deploy.ps1 -ApplyLayout   # layout\ の全プロファイル（Default / AI）も上書き（旧ファイルは .bak-日時 で残る）
pwsh tools\make-layouts.ps1             # layout\*.json を作り直す
pwsh tools\install-hooks.ps1            # Claude Code / Codex にフックを追加（-Uninstall で削除）
```

ログ: `plugin\local.virtualdesktop.sdPlugin\vdesk.log`（プラグイン。1つ前の起動分は `vdesk.prev.log`）、`%LOCALAPPDATA%\opendeck\logs\OpenDeck.log`（OpenDeck）

### Claude デスクトップアプリから作業するときの注意

Claude アプリは MSIX パッケージなので、アプリ内から起動したプロセスが `%APPDATA%` / `%LOCALAPPDATA%` に書くと
`%LOCALAPPDATA%\Packages\Claude_*\LocalCache\` の私用コピーに振り替えられ、OpenDeck からは見えない。

- ソースとプラグイン本体はこの D: のフォルダに置き、OpenDeck の `plugins` フォルダからはジャンクションでつなぐ
- OpenDeck の再起動や `%APPDATA%\opendeck` への書き込みは `tools\outside.ps1` 経由（WMI で起動するとパッケージの外で動く）
- OpenDeck をアプリ内から直接起動しない（空の設定で別の OpenDeck が立ち上がる）
