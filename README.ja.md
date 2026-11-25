<table>
  <tr>
    <td align="center"><img src="https://img.shields.io/badge/release-v1.0.0-blue.svg" alt="release" height="20"></td>
    <td align="center"><img src="https://img.shields.io/badge/rust-%E2%89%A51.75-orange.svg" alt="rust" height="20"></td>
    <td align="center"><img src="https://img.shields.io/badge/tests-18%20passed-green.svg" alt="tests" height="20"></td>
    <td align="center"><img src="https://img.shields.io/badge/sapper.exe-5.5%20MiB-green.svg" alt="binary size" height="20"></td>
    <td align="center"><img src="https://img.shields.io/badge/store-JSON%20on%20disk-Built-in-purple.svg" alt="store" height="20"></td>
    <td align="center"><img src="https://img.shields.io/badge/clap%204%20%2B%20ureq%202-std-only%20deps-cyan.svg" alt="deps" height="20"></td>
    <td align="center"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="license" height="20"></td>
  </tr>
</table>

<br>

[简体中文](./README.md) · [English](./README.en.md) · [日本語](./README.ja.md)

# sapper-rust

`sapper` の Rust 書き換え版。上流は [andrewji8/sapper-v2](https://github.com/andrewji8/sapper-v2)（Python）。
fact-graph ワーキングメモリとマルチエージェント・コマンドデッキ全体を Rust に移植したもので、目標は：

1. **JSON ディスク形式の 1:1 互換**：`--store` ディレクトリ構成、`facts/*.json` / `intents/*.json` /
   `hints/*.json` / `project.json` / `.lock` はすべてそのまま。Python 版と Rust 版で同じ
   `.sapper/` ディレクトリを共有できます。
2. **CLI 挙動契約の保持**：`init / status / graph / validate / frontier / pick / fact / intent / hint /
   peers / send / capture / show / doc / recall / claim / release / done / complete / confirm / dispatch /
   brainstorm-pick / setup / teardown / view / flag / arena / kb / bench / command / fleet / sweep /
   auto / hook` 全 30 超サブコマンド。
3. **Windows ネイティブ対応**：ファイルロックは `LockFileEx`（fs2 クレートが自動切替）；
   `kb / graph / engine / arena / auto / bench / sweep / command` はすべて Windows で直接実行可能。
   tmux に依存する `peers / send / capture / fleet / command hire·fire` は Windows 上で明確な
   エラーを出すため、WSL または Linux ホストの利用を推奨します。

## ディレクトリ構成

```
sapper-rust/
├─ Cargo.toml
├─ Cargo.lock
├─ src/                  # 19 モジュール + main
│  ├─ main.rs
│  ├─ cli.rs             # clap サブコマンド分岐（約 40 サブコマンド）
│  ├─ store.rs           # JSON グラフ + クロスプラットフォームファイルロック
│  ├─ graph.rs           # 描画 / 検証 / frontier / recall / プロンプトテンプレート
│  ├─ engine.rs          # サブプロセス claude -p / SAPPER_BIN + キープール切替
│  ├─ arena.rs           # TSecBench HTTP クライアント（ureq）
│  ├─ kb.rs              # オフライン知識ベース
│  ├─ hooks.rs           # Claude Code sessionstart / userpromptsubmit ブリッジ
│  ├─ settings.rs        # .claude/settings.json への冪等なフック注入
│  ├─ peers.rs           # tmux 対等通信
│  ├─ tmux.rs            # tmux ブリッジ（Windows ではエラー）
│  ├─ dispatch.rs        # reason / explore / verify / blindspot / brainstorm
│  ├─ auto.rs            # 自動ループドライバ
│  ├─ bench.rs           # TSecBench 直列 + 並列ドライバ
│  ├─ command.rs         # フィールド・コマンダー
│  ├─ fleet.rs           # tmux ワーカー編成
│  ├─ sweep.rs           # チャレンジ間のラウンドロビン
│  ├─ view.rs            # HTML 可視化（静的 + tiny_http サーバー）
│  ├─ error.rs
│  └─ util.rs
├─ assets/
│  ├─ templates/         # reason/explore/verify/command/brainstorm/blindspot .md
│  ├─ kb/entries/        # kb001..kb056 オフライン知識
│  └─ view_template.html # ダークテーマのダッシュボード
└─ target/release/sapper.exe
```

リソース（`assets/templates`, `assets/kb`）は**実行可能ファイルのあるディレクトリから最大 8 階層**
上方向に `assets/*` を探して解決します。`cargo build` 直後の `target/release/sapper.exe` でも
配置し直したバイナリでも、`assets/` が到達範囲内なら正しくロードできます。環境変数
`SAPPER_KB_ROOT` または `--kb-root / --templates` フラグで場所を固定することもできます。

## 前提

- **Rust ツールチェーン**（rustc / cargo、1.75+ を推奨 1.80+）
- **PATH 上の `claude` CLI**（または `SAPPER_BIN` 環境変数で互換 CLI を指定）
- 競技系コマンドは `BENCHMARK_BASE_URL` + `BENCHMARK_TOKEN`、または `--store` ディレクトリ内の
  `arena.json` が必要

## ビルドとスモークテスト

```powershell
cd D:\rust\projects\sapper-rust
cargo build --release
# または開発モード
cargo build
```

成果物：`target\release\sapper.exe`

スモークテスト（オフライン、LLM 不要）：

```powershell
$st = "$env:TEMP\sapper-smoke\.sapper"
Remove-Item $st -Recurse -Force -ErrorAction SilentlyContinue
$bin = ".\target\release\sapper.exe"

& $bin --store $st init --goal "capture the flag" --origin "smoke-test"
& $bin --store $st fact "target host" -k host -d addr=10.0.0.5 --creator tester -t main-host
& $bin --store $st intent "scan services on main-host" --from f001 --creator tester
& $bin --store $st claim i001 --worker tester
& $bin --store $st done i001 --fact "found ssh on 10.0.0.5" -k service -d port=22 --worker tester
& $bin --store $st status
& $bin --store $st validate
& $bin --store $st graph
& $bin --store $st recall "scan services"

& $bin kb list | Select-Object -First 5
& $bin kb search "sql injection"
```

期待される出力：

```
initialized fact graph at C:\...\sapper-smoke\.sapper
f001
i001
claimed i001 -> tester
i001 -> f002  (done)

project: proj  status: active
origin: smoke-test
goal:   capture the flag
counts: 3 facts, 0 open / 1 done intents, 0 hints
kinds:  1 hosts, 1 services

graph valid: ...

kb001  [web,recon]  Web 情報収集ワークフロー
kb002  [web,sqli]   SQL インジェクション…
...
```

## Python 版と `.sapper/` を共有する

- ノード JSON は 2 スペースインデント + 末尾改行、キー順序は `serde_json::preserve_order` で保持。
- 書き込みはすべて一時ファイル + `fsync` + 原子 `rename` を経て、Python 側
  `tempfile.mkstemp + os.replace` と同じセマンティクス。
- ファイルロックは Windows では `LockFileEx` 独占ロック（`fs2` クレートが自動切替し、
  `flock(LOCK_EX)` と同等の排他意味）。
- Python 側が書いた `facts/f001.json`、`intents/i001.json` は Rust 側の
  `sapper status / graph / recall / dispatch / auto` がそのまま読めます。逆も同じ。

## サブコマンド早見表

| 分類 | コマンド |
|------|------|
| グラフ | `init`, `status`, `graph`, `validate`, `frontier`, `pick`, `show` |
| ノード | `fact`, `intent`, `hint`, `doc`, `recall` |
| ライフサイクル | `claim`, `release`, `done`, `complete`, `confirm` |
| 自動 | `auto`（reason→explore→verify ループ） |
| LLM 分岐 | `dispatch reason/explore/verify/blindspot/brainstorm`, `brainstorm-pick` |
| 知識 | `kb list/search/show/add/import/root` |
| 競技 | `arena list/start/hint/submit/close`, `flag`, `bench`, `sweep`, `command` |
| 編成 | `fleet init/up/down/status/hire/fire/workers`, `peers`, `send`, `capture` |
| 統合 | `setup`, `teardown`, `hook sessionstart/userpromptsubmit`, `view` |

`--help` / `<サブコマンド> --help` で引数を参照できます。

## 環境変数

| 変数 | 用途 |
|------|------|
| `SAPPER_BIN` | エンジンの CLI パスまたは名称（既定 `claude`） |
| `SAPPER_KEY_POOL` | JSON 配列：`[{"base_url":"...","api_key":"..."},...]` による切替 |
| `SAPPER_RETRY_MODEL` | 429/クォータ枯渇時のフォールバックモデル |
| `SAPPER_KB_ROOT` | 知識ベースのルート（既定：`assets/kb` または CWD の `kb/` を自動検出） |
| `BENCHMARK_BASE_URL`, `BENCHMARK_TOKEN` | TSecBench / Arena エンドポイント |

## Windows に関する注意

- **tmux 依存**：`peers / send / capture / fleet up/down/hire/fire` は Windows ネイティブ環境で
  `tmux` を呼び出せません。コマンドは明示的なエラー（WSL へのヒント付き）を出力します。
  完全なマルチエージェント編成には、WSL 内の Linux sapper または Linux/WSL 上のビルドを利用してください。
- **`view` サーバー**：静的 HTML の `view` と `view --serve` の両方が利用できます。`--serve` は
  `tiny_http` でローカルポートにリッスンし、既定では 127.0.0.1 にバインドします。
- **ファイルロック**：Windows では fs2 が `LockFileEx` を使用し、`flock(LOCK_EX)` と排他意味を揃えています。
- **`sapper setup / teardown`**：`.claude/settings.json` への 2 フックの注入/削除を冪等に実行します。
  初回書き込み時は `settings.json.bak` にバックアップします。

## ビルド成果物と配布

`cargo build --release` は `lto` + `strip` を有効にしています。配布時は
`target/release/sapper.exe` と `assets/` を同梱してください：

```
sapper-1.0.0-win-x86_64/
├─ sapper.exe
└─ assets/
   ├─ templates/*.md
   ├─ kb/entries/kb*.json
   └─ view_template.html
```

exe は上方向に `assets/` を探します。相対位置に依存したくない場合は、リソースを exe と同じディレクトリ
（`kb/` と `templates/`）に置けば検出されます。あるいは `SAPPER_KB_ROOT` 環境変数で明示的に指定してください。

## 上流との対応

Python 版 `sapper-v2/lib/sapper.py`（約 4700 行）と本リポジトリの対応関係：

| Python 領域 | Rust モジュール |
|-------------|-----------|
| `Store` | `store.rs` |
| `cmd_status / cmd_graph / cmd_recall / cmd_validate / cmd_frontier / …` | `cli.rs` + `graph.rs` |
| `cmd_dispatch / apply_reason / apply_explore / apply_verify / …` | `dispatch.rs` + `engine.rs` |
| `cmd_auto` | `auto.rs` |
| `cmd_bench / _bench_parallel / _bench_submit_recorded` | `bench.rs` |
| `cmd_command / command_fleet_cfg` | `command.rs` |
| `cmd_fleet` | `fleet.rs` + `tmux.rs` |
| `cmd_sweep` | `sweep.rs` |
| `ArenaClient` | `arena.rs` |
| `KnowledgeBase / import_markdown` | `kb.rs` |
| `sapper-hook.py` | `hooks.rs` + `settings.rs` |
| `_settings_patch.py` | `settings.rs` |
| `cmd_view / view_serve` | `view.rs` |

## インストールガイド

1. **Rust ツールチェーンのインストール**（未インストールの場合）：

   ```powershell
   # rustup のインストール（Windows）
   winget install Rustlang.Rustup
   # 新規ターミナルで確認
   rustc --version   # 1.75+、1.80+ 推奨
   cargo --version
   ```

2. **エンジン CLI のインストール**：`claude` を PATH に（`npm install -g @anthropic-ai/claude-code`
   または社内配信版）。`SAPPER_BIN` で互換 CLI を指定することもできます。

3. **ビルド**：

   ```powershell
   cd D:\rust\projects\sapper-rust
   cargo build --release
   # 成果物: target\release\sapper.exe（約 5.5 MiB）
   ```

4. **（任意）PATH への追加**：どのディレクトリからでも `sapper` が使えるようになります：

   ```powershell
   [Environment]::SetEnvironmentVariable("Path", $env:Path + ";D:\rust\projects\sapper-rust\target\release", "User")
   # 新規ターミナルで：
   sapper --version   # 期待: sapper 1.0.0
   ```

   注意：`sapper.exe` は**自分自身のディレクトリから最大 8 階層上**を探索して `assets/`
   （テンプレート + 知識ベース）を探します。exe をプロジェクト外に配置して配布する場合は
   `assets/` と同梱してください（「ビルド成果物と配布」参照）、または `SAPPER_KB_ROOT` を設定してください。

5. **インストール確認**：

   ```powershell
   sapper kb list | Select-Object -First 3   # kb001.. が表示されれば知識ベース読込成功
   ```

## 使用方法

**典型ワークフロー**（CTF / 渗透題目を 0 から 1 へ）：

```powershell
cd D:\your\challenge            # 題目ディレクトリ
sapper setup                     # .sapper/ 作成 + Claude Code フック登録（任意）
sapper init --goal "capture the flag" --origin "ctf-2026"
sapper fact "標的 IP" -k host -d addr=10.10.10.10 -t target
sapper intent "ポートスキャン" --from f001
sapper auto --minutes 30        # reason→explore→verify 自動ループ（claude を呼ぶ）
sapper view                     # ブラウザで進捗を確認
sapper status                   # ターミナルで快速確認
sapper complete                 # 目標達成後に収束
```

**手動推進**（自動なし）：

```powershell
sapper frontier                 # 着手可能な intent
sapper claim i001 --worker me   # 1 件を認領
sapper done i001 --fact "ssh 22 ポート開放" -k service -d port=22 -t ssh
sapper dispatch explore --intent i001 --dry-run   # 実行前にプロンプト確認
sapper recall "sql injection"   # 過去に類似ノード探索
```

**競技モード**（TSecBench）：

```powershell
$env:BENCHMARK_BASE_URL = "https://bench.example.com"
$env:BENCHMARK_TOKEN    = "tok..."
sapper arena list                     # 題目リスト
sapper bench --minutes 60 --parallel 2  # 並列解答 + 自動 flag 提出
sapper flag "flag{xxx}" --challenge c1 --submit
```

**Claude Code 統合**：

```powershell
sapper setup      # sessionstart / userpromptsubmit の 2 フック注入
sapper teardown   # 削除（--purge を付けると .sapper/ も削除）
```

## 罠回避ガイド

| 罠 | 症状 | 対処 |
|----|------|------|
| `--from` に fact の**タイトル**を渡す | `from fact main-host does not exist` | `--from` には **fact id**（`f001`、`goal`）を。`-t` のタイトルではない。id は `sapper status` / `graph` で先確認 |
| description を `-d` に埋める | `unexpected argument '-d'` | description は**位置引数**：`sapper fact "説明テキスト" -k host -d addr=...`。`-d` は `key=value` データ |
| Windows で `peers / fleet` が失敗 | `tmux not available` | 予定の graceful degradation：これらのコマンドは tmux 依存。Windows ネイティブは `sapper auto`（サブプロセス方式）、完全な多窓編成は WSL/Linux で |
| 中国語が文字化け（`Web 淇℃伛...`） | PowerShell の GBK コードページで UTF-8 出力を表示 | セッションで `chcp 65001` または `Set-ConsoleOutputEncoding [System.Text.Encoding]::UTF8`。ファイル自体は正常（UTF-8, BOM なし） |
| `kb list` が空 / エントリ不在 | `no kb entries at ...` | 知識ベースルート解決順：`SAPPER_KB_ROOT` → exe から 8 階層上を `assets/kb` / `kb` 探索。exe を深いディレクトリにコピーした場合は `SAPPER_KB_ROOT` を `assets/kb` に設定 |
| ロック `os error 5`（旧ビルド） | `cannot lock ...: Access is denied` | 修正済み（Windows のロックファイルは write 権限で開く）。再発時は残留 `sapper` プロセスを kill / AV が `.lock` をロックしていないか確認 |
| dispatch タイムアウト | `engine dispatch timed out` | `--timeout` 増量（既定 600s）；クォータ枯渇時は `SAPPER_RETRY_MODEL` フォールバック + `SAPPER_KEY_POOL` 複数キー切替 |
| エンジンが非 JSON を返す | `did not return JSON` | `claude` が未ログイン / 旧版本。先に `claude -p "hi" --output-format json` で確認 |
| `view --serve` がポート占有 | `could not bind` | 別ポート：`sapper view --serve --port 8090`。127.0.0.1 以外へのバインドは警告を出力 |
| golden ファイル編集後にテストが落ちる | バイトアサーション不一致 | Python で再生成：`python tests\golden\make_golden.py`；出力は `tests/golden/` に LF 改行で（`.gitattributes` が固定） |
| 2 プロセスが同一 `.sapper/` を書き込み | ときどき `skipping ... file` 警告 | 正常：読み取り経路は破損 / 半書込ファイルは警告付きでスキップしてクラッシュしない。書き込みは独占ロックを持つため、前プロセス完了を待つ |
| `intent --confirm` 権限 | `conclusion permission denied` | `--confirm` intent は作成者または認領済み worker だけが conclude 可；先に `sapper claim`、その後に `done` |

## テスト

```powershell
cargo test --release     # ユニットテスト 14 + 統合テスト 4
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

| テスト群 | 範囲 |
|----------|------|
| `store::tests` | id 検証、ノードスキーマ、kind 正規化 |
| `util::tests` | deadline 解析、制御文字スクラブ、id 検証 |
| `util::tests::golden_*`（5） | **バイトレベルの Python 互換ガード**：`dump_json` の出力が `tests/golden/*.json` とバイト単位で一致（`ensure_ascii=False` の CJK 原文、キー順序、2 スペースインデント、末尾改行） |
| `tests/smoke.rs`（4） | CLI エンドツーエンド：init→fact→intent→done→validate 全フロー；破損 JSON への耐性；非 object ノードへの耐性；JSON キー順序/末尾改行 |

Golden ファイルは `tests/golden/make_golden.py`（Python 標準ライブラリ
`json.dump(ensure_ascii=False, indent=2, sort_keys=False) + "\n"`）で生成します。
`.gitattributes` が LF 改行を固定し、Windows クローン時の CRLF 変換でバイトアサーションが壊れない
よう保護します。再生成：`python tests\golden\make_golden.py`（リポジトリのルートで実行するか、
`golden/` の出力を手動で `tests/golden/` にコピーし直してください）。

## ライセンス

上流 sapper-v2 と同じ。
