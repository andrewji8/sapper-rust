# sapper-rust

`sapper` 的 Rust 重写版本。上游是 [andrewji8/sapper-v2](https://github.com/andrewji8/sapper-v2)（Python）。
这个仓库把整套 fact-graph 工作记忆 + 多智能体指挥台搬进 Rust，目标：

1. **JSON 磁盘格式 1:1 兼容**：`--store` 目录布局、`facts/*.json` / `intents/*.json` / `hints/*.json` /
   `project.json` / `.lock` 全部保留，Python 版与 Rust 版可以共用同一个 `.sapper/` 目录。
2. **CLI 行为契约保留**：`init / status / graph / validate / frontier / pick / fact / intent / hint /
   peers / send / capture / show / doc / recall / claim / release / done / complete / confirm / dispatch /
   brainstorm-pick / setup / teardown / view / flag / arena / kb / bench / command / fleet / sweep /
   auto / hook` 约 30 个子命令。
3. **Windows 原生支持**：文件锁走 `LockFileEx`（fs2 crate 自动切换）；`kb / graph / engine / arena /
   auto / bench / sweep / command` 全部可以在 Windows 上直接跑。依赖 tmux 的 `peers / send / capture /
   fleet / command hire·fire` 在 Windows 上会给出明确错误，建议配 WSL 或 Linux 主机。

## 目录布局

```
sapper-rust/
├─ Cargo.toml
├─ Cargo.lock
├─ src/                  # 19 个模块 + main
│  ├─ main.rs
│  ├─ cli.rs             # clap 子命令分发（~40 subcommands）
│  ├─ store.rs           # JSON 图 + 跨平台文件锁
│  ├─ graph.rs           # 渲染 / 校验 / frontier / recall / prompt 模板
│  ├─ engine.rs          # 子进程调用 claude -p / SAPPER_BIN + key pool 轮换
│  ├─ arena.rs           # TSecBench HTTP 客户端（ureq）
│  ├─ kb.rs              # 离线知识库
│  ├─ hooks.rs           # Claude Code sessionstart / userpromptsubmit 桥
│  ├─ settings.rs        # .claude/settings.json 幂等 hook 注入
│  ├─ peers.rs           # tmux 对等通信
│  ├─ tmux.rs            # tmux 桥（Windows 下报错）
│  ├─ dispatch.rs        # reason / explore / verify / blindspot / brainstorm
│  ├─ auto.rs            # 自动 loop 驱动
│  ├─ bench.rs           # TSecBench 串行 + 并行驱动
│  ├─ command.rs         # field commander
│  ├─ fleet.rs           # tmux worker 编排
│  ├─ sweep.rs           # 轮询多 challenge
│  ├─ view.rs            # HTML 可视化（静态 + tiny_http 服务）
│  ├─ error.rs
│  └─ util.rs
├─ assets/
│  ├─ templates/         # reason/explore/verify/command/brainstorm/blindspot .md
│  ├─ kb/entries/        # kb001..kb056 离线知识
│  └─ view_template.html # 暗色主题 dashboard
└─ target/release/sapper.exe
```

资源文件（`assets/templates`, `assets/kb`）会通过 **exe 所在目录向上最多 8 级** 找 `assets/*`，
所以无论是 `cargo build` 出来的 `target/release/sapper.exe` 还是被拷贝到别处的可执行文件，
只要 `assets/` 目录还能找到，就都能正常加载知识库和 prompt 模板。也可以用 `SAPPER_KB_ROOT` 环境变量
或 `--kb-root / --templates` 参数强制指定。

## 前置

- **Rust 工具链**（rustc / cargo，1.75+ 建议 1.80+）
- **`claude` CLI 在 PATH 上**（或 `SAPPER_BIN` 环境变量指向任意兼容 CLI）
- 竞争相关命令需要 `BENCHMARK_BASE_URL` + `BENCHMARK_TOKEN`，或者 `--store` 目录下的 `arena.json`

## 构建 & 冒烟测试

```powershell
cd D:\rust\projects\sapper-rust
cargo build --release
# 或者开发模式
cargo build
```

产物：`target\release\sapper.exe`

冒烟（无网络、无 LLM）：

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

期望输出：

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

kb001  [web,recon]  Web 信息收集工作流
kb002  [web,sqli]   SQL 注入 …
...
```

## 与 Python 版共用一个 `.sapper/`

- 节点 JSON 用 2 空格缩进 + 尾部换行，键序通过 `serde_json::preserve_order` 保序。
- 每次写入走临时文件 + `fsync` + 原子 `rename`，与 Python 端 `tempfile.mkstemp + os.replace` 语义一致。
- 文件锁在 Windows 上是 `LockFileEx` 独占锁（`fs2` crate 自动切换），跨进程跨平台互斥。
- Python 侧写过的 `facts/f001.json`、`intents/i001.json` 直接可以被 Rust 侧 `sapper status / graph /
  recall / dispatch / auto` 读取，反之亦然。

## 主要子命令速查

| 类别 | 命令 |
|------|------|
| 图 | `init`, `status`, `graph`, `validate`, `frontier`, `pick`, `show` |
| 节点 | `fact`, `intent`, `hint`, `doc`, `recall` |
| 生命周期 | `claim`, `release`, `done`, `complete`, `confirm` |
| 自动 | `auto`（reason→explore→verify 循环） |
| LLM 分发 | `dispatch reason/explore/verify/blindspot/brainstorm`, `brainstorm-pick` |
| 知识 | `kb list/search/show/add/import/root` |
| 比赛 | `arena list/start/hint/submit/close`, `flag`, `bench`, `sweep`, `command` |
| 编排 | `fleet init/up/down/status/hire/fire/workers`, `peers`, `send`, `capture` |
| 集成 | `setup`, `teardown`, `hook sessionstart/userpromptsubmit`, `view` |

`--help` / `<subcommand> --help` 可查具体参数。

## 环境变量

| 变量 | 用途 |
|------|------|
| `SAPPER_BIN` | 引擎 CLI 路径或名称（默认 `claude`） |
| `SAPPER_KEY_POOL` | JSON 数组：`[{"base_url":"...","api_key":"..."},...]` 轮换 |
| `SAPPER_RETRY_MODEL` | 429/额度用尽时降级模型 |
| `SAPPER_KB_ROOT` | 知识库根目录（默认自动找 `assets/kb` 或 CWD `kb/`） |
| `BENCHMARK_BASE_URL`, `BENCHMARK_TOKEN` | TSecBench / Arena 端点 |

## Windows 注意事项

- **tmux 相关**：`peers / send / capture / fleet up/down/hire/fire` 在 Windows 原生环境无法调用
  `tmux`。命令会打印明确错误（含 WSL 提示）。要跑完整多 agent 编排，请使用 WSL 里的 Linux
  sapper 或直接在 Linux/WSL 上构建。
- **`view` 服务**：`view` 静态 HTML 与 `view --serve` 都可用；`--serve` 用 `tiny_http` 监听本地端口，
  默认绑到 127.0.0.1。
- **文件锁**：Windows 上 fs2 自动走 `LockFileEx`，独占语义与 `flock(LOCK_EX)` 对齐。
- **`sapper setup / teardown`**：在 `.claude/settings.json` 中幂等注入/移除两条 hook。首次写入会
  备份为 `settings.json.bak`。

## 构建产物 & 分发

`cargo build --release` 已启用 `lto` + `strip`。发布时可把 `target/release/sapper.exe` 与
`assets/` 一起打包：

```
sapper-1.0.0-win-x86_64/
├─ sapper.exe
└─ assets/
   ├─ templates/*.md
   ├─ kb/entries/kb*.json
   └─ view_template.html
```

exe 会向上找 `assets/`。若不想依赖相对位置，把资源放到 exe 同目录下（`kb/` 和 `templates/`）也能被
识别，或者显式通过 `SAPPER_KB_ROOT` 环境变量指定。

## 上游对照

Python 版 `sapper-v2/lib/sapper.py`（约 4700 行）在本仓库中的对应关系：

| Python 区域 | Rust 模块 |
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

## 测试

```powershell
cargo test --release     # 14 单元测试 + 4 集成测试
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

| 测试组 | 覆盖 |
|--------|------|
| `store::tests` | id 校验、节点 schema、kind 归一化 |
| `util::tests` | deadline 解析、控制字符清洗、id 校验 |
| `util::tests::golden_*`（5） | **字节级 Python 兼容守护**：`dump_json` 输出与 `tests/golden/*.json` 逐字节一致（`ensure_ascii=False` 的 CJK 原文、键序、2 空格缩进、尾部换行） |
| `tests/smoke.rs`（4） | CLI 端到端：init→fact→intent→done→validate 全链路；损坏 JSON 容错；非 object 节点容错；JSON 键序/尾换行 |

Golden 文件由 `tests/golden/make_golden.py`（Python stdlib `json.dump(ensure_ascii=False, indent=2, sort_keys=False) + "\n"`）生成，`.gitattributes` 固定为 LF 行尾，防止 Windows 克隆时 CRLF 改写破坏字节断言。重新生成：`python tests\golden\make_golden.py`（需与仓库根目录对齐后运行，或手动把 `golden/` 输出拷回 `tests/golden/`）。

## 许可

与上游 sapper-v2 一致。


