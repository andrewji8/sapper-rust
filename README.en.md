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

Rust rewrite of `sapper`. Upstream: [andrewji8/sapper-v2](https://github.com/andrewji8/sapper-v2) (Python).
This repo ports the entire fact-graph working memory + multi-agent command deck into Rust, with three goals:

1. **1:1 JSON disk-format compatibility**: the `--store` layout, `facts/*.json` / `intents/*.json` /
   `hints/*.json` / `project.json` / `.lock` all stay the same — the Python and Rust versions can
   share a single `.sapper/` directory.
2. **CLI behavior contract preserved**: `init / status / graph / validate / frontier / pick / fact /
   intent / hint / peers / send / capture / show / doc / recall / claim / release / done / complete /
   confirm / dispatch / brainstorm-pick / setup / teardown / view / flag / arena / kb / bench /
   command / fleet / sweep / auto / hook` — ~30 subcommands.
3. **Native Windows support**: file locking via `LockFileEx` (the `fs2` crate switches
   automatically); `kb / graph / engine / arena / auto / bench / sweep / command` all run directly
   on Windows. tmux-dependent commands (`peers / send / capture / fleet / command hire·fire`)
   fail with clear errors on Windows — use WSL or a Linux host for those.

## Layout

```
sapper-rust/
├─ Cargo.toml
├─ Cargo.lock
├─ src/                  # 19 modules + main
│  ├─ main.rs
│  ├─ cli.rs             # clap subcommand dispatch (~40 subcommands)
│  ├─ store.rs           # JSON graph + cross-platform file locking
│  ├─ graph.rs           # rendering / validation / frontier / recall / prompt templates
│  ├─ engine.rs          # subprocess `claude -p` / SAPPER_BIN + key-pool rotation
│  ├─ arena.rs           # TSecBench HTTP client (ureq)
│  ├─ kb.rs              # offline knowledge base
│  ├─ hooks.rs           # Claude Code sessionstart / userpromptsubmit bridge
│  ├─ settings.rs        # idempotent hook injection into .claude/settings.json
│  ├─ peers.rs           # tmux peer messaging
│  ├─ tmux.rs            # tmux bridge (errors clearly on Windows)
│  ├─ dispatch.rs        # reason / explore / verify / blindspot / brainstorm
│  ├─ auto.rs            # auto-loop driver
│  ├─ bench.rs           # TSecBench serial + parallel drivers
│  ├─ command.rs         # field commander
│  ├─ fleet.rs           # tmux worker orchestration
│  ├─ sweep.rs           # round-robin across challenge graphs
│  ├─ view.rs            # HTML visualization (static + tiny_http server)
│  ├─ error.rs
│  └─ util.rs
├─ assets/
│  ├─ templates/         # reason/explore/verify/command/brainstorm/blindspot .md
│  ├─ kb/entries/        # kb001..kb056 offline knowledge
│  └─ view_template.html # dark-theme dashboard
└─ target/release/sapper.exe
```

Assets (`assets/templates`, `assets/kb`) are discovered by walking **up to 8 levels from the
executable's directory** to find `assets/*`, so both a freshly built `target/release/sapper.exe`
and a relocated binary load the knowledge base and prompt templates correctly. You can also pin
locations with the `SAPPER_KB_ROOT` env var or `--kb-root / --templates` flags.

## Prerequisites

- **Rust toolchain** (rustc / cargo, 1.75+, 1.80+ recommended)
- **`claude` CLI on PATH** (or point `SAPPER_BIN` at any compatible CLI)
- Competition commands need `BENCHMARK_BASE_URL` + `BENCHMARK_TOKEN`, or an `arena.json` under
  the `--store` directory

## Build & smoke test

```powershell
cd D:\rust\projects\sapper-rust
cargo build --release
# or dev mode
cargo build
```

Artifact: `target\release\sapper.exe`

Smoke test (offline, no LLM):

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

Expected output:

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

kb001  [web,recon]  Web information gathering workflow
kb002  [web,sqli]   SQL injection ...
...
```

## Sharing a `.sapper/` with the Python version

- Node JSON is 2-space indented with a trailing newline; key order is preserved via
  `serde_json::preserve_order`.
- Every write goes through a temp file + `fsync` + atomic `rename`, matching the Python
  `tempfile.mkstemp + os.replace` semantics.
- The file lock is an exclusive `LockFileEx` lock on Windows (the `fs2` crate switches
  automatically), mutually exclusive across processes and platforms.
- `facts/f001.json` and `intents/i001.json` written by the Python side are read directly by the
  Rust side's `sapper status / graph / recall / dispatch / auto` — and vice versa.

## Subcommand cheat sheet

| Category | Commands |
|----------|----------|
| Graph | `init`, `status`, `graph`, `validate`, `frontier`, `pick`, `show` |
| Nodes | `fact`, `intent`, `hint`, `doc`, `recall` |
| Lifecycle | `claim`, `release`, `done`, `complete`, `confirm` |
| Autonomy | `auto` (reason→explore→verify loop) |
| LLM dispatch | `dispatch reason/explore/verify/blindspot/brainstorm`, `brainstorm-pick` |
| Knowledge | `kb list/search/show/add/import/root` |
| Competition | `arena list/start/hint/submit/close`, `flag`, `bench`, `sweep`, `command` |
| Orchestration | `fleet init/up/down/status/hire/fire/workers`, `peers`, `send`, `capture` |
| Integration | `setup`, `teardown`, `hook sessionstart/userpromptsubmit`, `view` |

Run `--help` / `<subcommand> --help` for the full argument list.

## Environment variables

| Variable | Purpose |
|----------|---------|
| `SAPPER_BIN` | Engine CLI path or name (default `claude`) |
| `SAPPER_KEY_POOL` | JSON array: `[{"base_url":"...","api_key":"..."},...]` for rotation |
| `SAPPER_RETRY_MODEL` | Fallback model on 429 / quota exhaustion |
| `SAPPER_KB_ROOT` | Knowledge base root (default: auto-detect `assets/kb` or CWD `kb/`) |
| `BENCHMARK_BASE_URL`, `BENCHMARK_TOKEN` | TSecBench / Arena endpoints |

## Windows notes

- **tmux-dependent**: `peers / send / capture / fleet up/down/hire/fire` cannot invoke `tmux`
  on native Windows. The commands print clear errors (with a WSL hint). For full multi-agent
  orchestration, run the Linux sapper inside WSL or build on Linux/WSL.
- **`view` server**: both the static HTML `view` and `view --serve` work; `--serve` listens on
  a local port via `tiny_http`, bound to 127.0.0.1 by default.
- **File locking**: `fs2` uses `LockFileEx` on Windows; exclusive semantics match `flock(LOCK_EX)`.
- **`sapper setup / teardown`**: idempotently inject/remove the two hooks in `.claude/settings.json`.
  The first write backs up to `settings.json.bak`.

## Packaging & distribution

`cargo build --release` enables `lto` + `strip`. For release, ship `target/release/sapper.exe`
alongside `assets/`:

```
sapper-1.0.0-win-x86_64/
├─ sapper.exe
└─ assets/
   ├─ templates/*.md
   ├─ kb/entries/kb*.json
   └─ view_template.html
```

The exe walks upward to find `assets/`. If you don't want the relative lookup, place the resources
next to the exe (`kb/` and `templates/`) — also detected — or set `SAPPER_KB_ROOT` explicitly.

## Upstream mapping

Mapping of the Python `sapper-v2/lib/sapper.py` (~4700 lines) to this repo:

| Python area | Rust module |
|-------------|-------------|
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

## Installation

1. **Install the Rust toolchain** (if missing):

   ```powershell
   # install rustup (Windows)
   winget install Rustlang.Rustup
   # verify in a new terminal
   rustc --version   # 1.75+, 1.80+ recommended
   cargo --version
   ```

2. **Install the engine CLI**: `claude` on PATH (`npm install -g @anthropic-ai/claude-code`,
   or your company's distribution). You can also point `SAPPER_BIN` at any compatible CLI.

3. **Build**:

   ```powershell
   cd D:\rust\projects\sapper-rust
   cargo build --release
   # artifact: target\release\sapper.exe (~5.5 MiB)
   ```

4. **(Optional) add to PATH** so `sapper` works from any directory:

   ```powershell
   [Environment]::SetEnvironmentVariable("Path", $env:Path + ";D:\rust\projects\sapper-rust\target\release", "User")
   # in a new terminal:
   sapper --version   # expect: sapper 1.0.0
   ```

   Note: `sapper.exe` walks **up to 8 levels from its own directory** to find `assets/`
   (templates + knowledge base). When shipping the exe outside the project, bundle `assets/`
   with it (see "Packaging & distribution") or set `SAPPER_KB_ROOT`.

5. **Verify the install**:

   ```powershell
   sapper kb list | Select-Object -First 3   # seeing kb001.. means the KB loaded
   ```

## Usage

**Typical workflow** (a CTF / pentest engagement from zero):

```powershell
cd D:\your\challenge            # engagement directory
sapper setup                     # create .sapper/ + register Claude Code hooks (optional)
sapper init --goal "capture the flag" --origin "ctf-2026"
sapper fact "target IP" -k host -d addr=10.10.10.10 -t target
sapper intent "port scan" --from f001
sapper auto --minutes 30        # autonomous reason -> explore -> verify loop (calls claude)
sapper view                     # watch progress in the browser
sapper status                   # quick terminal view
sapper complete                 # wrap up once the goal is met
```

**Manual stepping** (no autonomy):

```powershell
sapper frontier                 # intents ready to work
sapper claim i001 --worker me   # claim one
sapper done i001 --fact "ssh port 22 open" -k service -d port=22 -t ssh
sapper dispatch explore --intent i001 --dry-run   # inspect the prompt before running
sapper recall "sql injection"   # find historically similar nodes
```

**Competition mode** (TSecBench):

```powershell
$env:BENCHMARK_BASE_URL = "https://bench.example.com"
$env:BENCHMARK_TOKEN    = "tok..."
sapper arena list                     # list challenges
sapper bench --minutes 60 --parallel 2   # parallel solving + automatic flag submission
sapper flag "flag{xxx}" --challenge c1 --submit
```

**Claude Code integration**:

```powershell
sapper setup      # inject the sessionstart / userpromptsubmit hooks
sapper teardown   # remove them (add --purge to also delete .sapper/)
```

## Pitfall guide

| Pitfall | Symptom | Fix |
|---------|---------|-----|
| Passing a fact **title** to `--from` | `from fact main-host does not exist` | `--from` needs the **fact id** (`f001`, `goal`), not the `-t` title. Check ids with `sapper status` / `graph` first |
| Putting the description into `-d` | `unexpected argument '-d'` | The description is a **positional argument**: `sapper fact "description text" -k host -d addr=...`. `-d` is `key=value` data |
| `peers / fleet` failing on Windows | `tmux not available` | Expected graceful degradation: these commands need tmux. On native Windows use `sapper auto` (subprocess mode); for full multi-window orchestration run WSL/Linux |
| Garbled CJK in the console (`Web 淇℃伛...`) | PowerShell's GBK codepage rendering UTF-8 output | `chcp 65001` or `Set-ConsoleOutputEncoding [System.Text.Encoding]::UTF8` in the session. The files themselves are fine (UTF-8, no BOM) |
| `kb list` empty / entries not found | `no kb entries at ...` | KB root resolution order: `SAPPER_KB_ROOT` -> walk up 8 levels from the exe for `assets/kb` / `kb`. If the exe was copied into a deep directory, set `SAPPER_KB_ROOT` to the `assets/kb` path |
| Lock `os error 5` (older builds) | `cannot lock ...: Access is denied` | Fixed (the Windows lock file now opens with write access). If it still happens: kill stale `sapper` processes / check whether AV is locking `.lock` |
| Dispatch timeout | `engine dispatch timed out` | Raise `--timeout` (default 600s); on quota exhaustion set `SAPPER_RETRY_MODEL` fallback + `SAPPER_KEY_POOL` multi-key rotation |
| Engine returns non-JSON | `did not return JSON` | `claude` not logged in / too old. Verify with `claude -p "hi" --output-format json` first |
| `view --serve` port in use | `could not bind` | Use another port: `sapper view --serve --port 8090`. Binding off 127.0.0.1 prints a plaintext warning |
| Golden test fails after editing fixtures | byte-assertion mismatch | Regenerate with Python: `python tests\golden\make_golden.py`; output stays in `tests/golden/` with LF endings (`.gitattributes` pins them) |
| Two processes writing one `.sapper/` | occasional `skipping ... file` warnings | Normal: the read path skips corrupt/half-written files with a warning instead of crashing; writers hold the exclusive lock, so just wait for the previous process to finish |
| `intent --confirm` permission | `conclusion permission denied` | A `--confirm` intent can only be concluded by its creator or a worker that claimed it; `sapper claim` first, then `done` |

## Testing

```powershell
cargo test --release     # 14 unit tests + 4 integration tests
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

| Test group | Coverage |
|------------|----------|
| `store::tests` | id validation, node schema, kind normalization |
| `util::tests` | deadline parsing, control-char scrubbing, id validation |
| `util::tests::golden_*` (5) | **byte-level Python compatibility guard**: `dump_json` output is byte-identical to `tests/golden/*.json` (raw CJK with `ensure_ascii=False`, key order, 2-space indent, trailing newline) |
| `tests/smoke.rs` (4) | CLI end-to-end: init→fact→intent→done→validate full flow; corrupt-JSON tolerance; non-object node tolerance; JSON key order / trailing newline |

Golden files are generated by `tests/golden/make_golden.py` (Python stdlib
`json.dump(ensure_ascii=False, indent=2, sort_keys=False) + "\n"`); `.gitattributes` pins LF
line endings so a Windows clone's CRLF rewriting cannot break the byte assertions. Regenerate:
`python tests\golden\make_golden.py` (run from the repo root, or copy the `golden/` output back
into `tests/golden/`).

## License

Same as upstream sapper-v2.
