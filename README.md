

# sapper-rust

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

## 安装指南

1. **安装 Rust 工具链**（如未安装）：

   ```powershell
   # 安装 rustup（Windows）
   winget install Rustlang.Rustup
   # 新开终端确认
   rustc --version   # 1.75+，建议 1.80+
   cargo --version
   ```

2. **安装引擎 CLI**：`claude` 命令在 PATH 上（`npm install -g @anthropic-ai/claude-code` 或公司内部发行版）。
   也可以用 `SAPPER_BIN` 指向任意兼容 CLI。

3. **构建**：

   ```powershell
   cd D:\rust\projects\sapper-rust
   cargo build --release
   # 产物: target\release\sapper.exe（约 5.5 MiB）
   ```

4. **（可选）加入 PATH**，之后任何目录都能直接 `sapper`：

   ```powershell
   [Environment]::SetEnvironmentVariable("Path", $env:Path + ";D:\rust\projects\sapper-rust\target\release", "User")
   # 新开终端
   sapper --version   # 期望: sapper 1.0.0
   ```

   注意：`sapper.exe` 会**向上最多 8 级**寻找 `assets/`（模板 + 知识库）。
   把 exe 移出项目目录之外分发时，请连 `assets/` 一起打包（见「构建产物 & 分发」），
   或设置 `SAPPER_KB_ROOT`。

5. **验证安装**：

   ```powershell
   sapper kb list | Select-Object -First 3   # 能看到 kb001.. 即知识库加载成功
   ```

## 使用方法

**典型工作流**（一个 CTF / 渗透题目从 0 到 1）：

```powershell
cd D:\your\challenge            # 题目目录
sapper setup                     # 创建 .sapper/ + 注册 Claude Code hooks（可选）
sapper init --goal "capture the flag" --origin "ctf-2026"
sapper fact "靶机 IP" -k host -d addr=10.10.10.10 -t target
sapper intent "端口扫描" --from f001
sapper auto --minutes 30        # 自动 reason→explore→verify 循环（调用 claude）
sapper view                     # 浏览器查看图形化进展
sapper status                   # 终端快速查看
sapper complete                 # 目标达成后收尾
```

**手动推进**（不自动）：

```powershell
sapper frontier                 # 看可开工的 intent
sapper claim i001 --worker me   # 认领
sapper done i001 --fact "ssh 22 端口开放" -k service -d port=22 -t ssh
sapper dispatch explore --intent i001 --dry-run   # 先看 prompt 再实跑
sapper recall "sql injection"   # 找历史相似节点
```

**比赛模式**（TSecBench）：

```powershell
$env:BENCHMARK_BASE_URL = "https://bench.example.com"
$env:BENCHMARK_TOKEN    = "tok..."
sapper arena list                     # 列题目
sapper bench --minutes 60 --parallel 2  # 并行求解 + 自动交 flag
sapper flag "flag{xxx}" --challenge c1 --submit
```

**Claude Code 集成**：

```powershell
sapper setup      # 注入 sessionstart / userpromptsubmit 两个 hook
sapper teardown   # 移除（加 --purge 连 .sapper/ 一起删）
```

## 避坑指南

| 坑 | 现象 | 解法 |
|----|------|------|
| `--from` 传了 fact 标题 | `from fact main-host does not exist` | `--from` 必须用 **fact id**（`f001`、`goal`），不是 `-t` 起的标题。先 `sapper status` / `graph` 查 id |
| 把 description 塞进 `-d` | `unexpected argument '-d'` | description 是**位置参数**：`sapper fact "描述文字" -k host -d addr=...`。`-d` 是 `key=value` 数据 |
| Windows 上 `peers / fleet` 报错 | `tmux not available` | 属正常降级：这些命令依赖 tmux。原生 Windows 上改用 `sapper auto`（子进程模式）；要完整多窗口编排请用 WSL/Linux |
| 控制台中文乱码（`Web 淇℃伛...`） | PowerShell 默认 GBK 代码页显示 UTF-8 输出 | 会话内执行 `chcp 65001` 或 `Set-ConsoleOutputEncoding [System.Text.Encoding]::UTF8`。文件本身没问题（JSON 是 UTF-8 无 BOM） |
| `kb list` 空 / 找不到条目 | `no kb entries at ...` | 知识库根解析顺序：`SAPPER_KB_ROOT` → exe 上溯 8 级找 `assets/kb` / `kb`。exe 被拷到深处目录时设 `SAPPER_KB_ROOT` 指向 `assets/kb` |
| 锁 `os error 5`（旧构建） | `cannot lock ...: Access is denied` | 已修复（Windows 锁文件需 write 权限）。若仍出现：杀残留 `sapper` 进程 / 检查杀毒软件锁定了 `.lock` |
| dispatch 超时 | `engine dispatch timed out` | 调大 `--timeout`（默认 600s）；额度不足配 `SAPPER_RETRY_MODEL` 降级模型 + `SAPPER_KEY_POOL` 多 key 轮换 |
| engine 返回非 JSON | `did not return JSON` | `claude` 未登录 / 版本过旧。先单独跑 `claude -p "hi" --output-format json` 验证 |
| `view --serve` 端口占用 | `could not bind` | 换端口：`sapper view --serve --port 8090`。非 127.0.0.1 绑定会有明文警告 |
| 改了 golden 文件后测试挂 | 字节断言失败 | 用 Python 重新生成：`python tests\golden\make_golden.py`，确认输出仍在 `tests/golden/` 且 LF 行尾（`.gitattributes` 已固定） |
| 双进程写同一 `.sapper/` | 偶发 `skipping ... file` 告警 | 正常：读路径对损坏/半写文件是告警跳过而非崩溃；写路径有独占锁，等前一进程结束即可 |
| `intent --confirm` 与权限 | `conclusion permission denied` | `--confirm` intent 只有创建者或已 claim 的 worker 可 conclude；先 `sapper claim` 再 `done` |

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
