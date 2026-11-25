//! fleet — launch/stop the whole formation in tmux panes (match-day).
//! Port of `cmd_fleet`. On Windows without tmux, `up`/`down`/`hire` degrade
//! to a clear error; `init` and `status` still work.

use crate::error::{Result, SapperError};
use crate::store::read_json;
use crate::tmux;
use serde_json::{json, Value};
use std::path::Path;

pub const FLEET_FILE: &str = "fleet.json";

pub fn fleet_template() -> Value {
    json!({
        "session": "sapper-fleet",
        "root": "runs",
        "deadline": "2026-10-17T13:00:00",
        "panes": [
            {"name": "commander", "type": "command", "extra_args": "--verify"},
            {"name": "board", "type": "shell", "command": "watch -n 30 'sapper sweep-status --root runs 2>/dev/null || ls runs'"},
        ],
        "_comment": "types: command (sapper command --root ROOT --deadline D), auto:<name> (sapper auto in runs/<name>), shell (any command). `sapper fleet up` opens the tmux session and starts everything; `down` kills it. Run `up` inside the match debugging window; the loops keep running unattended once the match starts."
    })
}

fn fleet_load(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Err(SapperError::new(format!(
            "no fleet config at {}; run `sapper fleet init` first",
            path.display()
        )));
    }
    let cfg = read_json(path)?;
    if !cfg.is_object() || cfg.get("session").and_then(|v| v.as_str()).is_none() {
        return Err(SapperError::new(format!(
            "fleet config {} needs a string 'session'",
            path.display()
        )));
    }
    let panes = cfg.get("panes");
    if !panes
        .map(|v| v.is_array() && !v.as_array().unwrap().is_empty())
        .unwrap_or(false)
    {
        return Err(SapperError::new(format!(
            "fleet config {} needs a non-empty 'panes' list",
            path.display()
        )));
    }
    Ok(cfg)
}

fn fleet_pane_command(cfg: &Value, pane: &Value, base_dir: Option<&Path>) -> Result<String> {
    let ptype = pane.get("type").and_then(|v| v.as_str()).unwrap_or("shell");
    let root = cfg.get("root").and_then(|v| v.as_str()).unwrap_or("runs");
    let deadline = cfg.get("deadline").and_then(|v| v.as_str());
    let engine = tmux::engine_prefix();
    let mut cmd;
    if ptype == "command" {
        cmd = format!("{engine} command --root {root}");
        if let Some(d) = deadline {
            cmd.push_str(&format!(" --deadline {d}"));
        }
        let extra = pane
            .get("extra_args")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !extra.is_empty() {
            cmd.push_str(extra);
        }
    } else if let Some(name) = ptype.strip_prefix("auto:") {
        cmd = format!("{engine} --store {root}/{name}/.sapper auto");
        if let Some(d) = deadline {
            cmd.push_str(&format!(" --deadline {d}"));
        }
        let extra = pane
            .get("extra_args")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !extra.is_empty() {
            cmd.push_str(extra);
        }
    } else if ptype == "shell" {
        let command = pane
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| SapperError::new("shell pane needs a 'command'"))?;
        if command.trim().is_empty() {
            return Err(SapperError::new("shell pane needs a 'command'"));
        }
        if let Some(base_dir) = base_dir {
            return Ok(format!("cd {} && {command}", base_dir.display()));
        }
        return Ok(command.to_string());
    } else {
        return Err(SapperError::new(format!(
            "unknown fleet pane type {ptype:?}"
        )));
    }
    if let Some(base_dir) = base_dir {
        cmd = format!("cd {} && {cmd}", base_dir.display());
    }
    Ok(cmd)
}

pub fn cmd_fleet(args: FleetArgs) -> Result<i32> {
    let path = Path::new(&args.file);
    match args.action.as_str() {
        "init" => {
            if path.exists() {
                return Err(SapperError::new(format!(
                    "fleet config already exists: {}",
                    path.display()
                )));
            }
            let mut s = serde_json::to_string_pretty(&fleet_template()).unwrap();
            s.push('\n');
            std::fs::write(path, s)?;
            println!(
                "wrote {} — edit deadline/panes, then `sapper fleet up`",
                path.display()
            );
            Ok(0)
        }
        "up" => {
            let cfg = fleet_load(path)?;
            let session = cfg["session"].as_str().unwrap_or("").to_string();
            if tmux::tmux_has_session(&session) {
                return Err(SapperError::new(format!(
                    "tmux session {session:?} already exists; `sapper fleet down` first"
                )));
            }
            tmux::tmux(&["new-session", "-d", "-s", &session], true)?;
            let mut first = true;
            for pane in cfg["panes"].as_array().unwrap() {
                let name = pane
                    .get("name")
                    .and_then(|v| v.as_str())
                    .or_else(|| pane.get("type").and_then(|v| v.as_str()))
                    .unwrap_or("")
                    .to_string();
                let command = fleet_pane_command(&cfg, pane, Some(path.parent().unwrap()))?;
                if first {
                    tmux::tmux(
                        &["rename-window", "-t", &format!("{session}:0"), &name],
                        true,
                    )?;
                    first = false;
                } else {
                    tmux::tmux(&["new-window", "-d", "-t", &session, "-n", &name], true)?;
                }
                // literal paste via buffer: avoids send-keys eating special chars
                let buf_path = std::env::temp_dir().join(format!(
                    ".sapper-fleet-{}-{}.sh",
                    session.replace('-', "_"),
                    std::process::id()
                ));
                std::fs::write(&buf_path, format!("{command}\n"))?;
                let buf_str = buf_path.to_string_lossy().to_string();
                tmux::tmux(&["load-buffer", &buf_str], true)?;
                let target = format!("{session}:{name}");
                tmux::tmux(&["paste-buffer", "-t", &target], true)?;
                tmux::tmux(&["send-keys", "-t", &target, "Enter"], true)?;
                let _ = std::fs::remove_file(&buf_path);
                println!("[fleet] {name}: {command}");
            }
            println!(
                "[fleet] session {session:?} up — detach with Ctrl-b d; loops run unattended. `sapper fleet down` to stop."
            );
            Ok(0)
        }
        "down" => {
            let cfg = fleet_load(path)?;
            let session = cfg["session"].as_str().unwrap_or("").to_string();
            if !tmux::tmux_has_session(&session) {
                println!("[fleet] session {session:?} not running");
                return Ok(0);
            }
            tmux::tmux(&["kill-session", "-t", &session], true)?;
            println!("[fleet] session {session:?} killed");
            Ok(0)
        }
        "hire" => {
            let cfg = if path.exists() {
                fleet_load(path)
                    .unwrap_or_else(|_| json!({"session": "sapper-fleet", "root": "runs"}))
            } else {
                json!({"session": "sapper-fleet", "root": "runs"})
            };
            let target = args
                .target
                .as_deref()
                .ok_or_else(|| SapperError::new("fleet hire needs --target <challenge>"))?;
            let minutes = args.minutes.unwrap_or(30);
            let session = cfg["session"].as_str().unwrap_or("sapper-fleet");
            let root = cfg["root"].as_str().unwrap_or("runs");
            let base_dir = std::env::current_dir().unwrap_or_default();
            let extra = args.extra_args.as_deref().unwrap_or("");
            let engine = tmux::engine_prefix();
            let spawned = tmux::hire_worker(
                session,
                root,
                target,
                minutes,
                args.max_steps.unwrap_or(8),
                extra,
                &base_dir,
                &engine,
            )
            .unwrap_or(false);
            let hire_msg = if spawned {
                format!("worker window w-{target} up")
            } else {
                "worker already running".to_string()
            };
            println!("[fleet] hire {target}: {hire_msg}");
            Ok(0)
        }
        "fire" => {
            let cfg = if path.exists() {
                fleet_load(path)
                    .unwrap_or_else(|_| json!({"session": "sapper-fleet", "root": "runs"}))
            } else {
                json!({"session": "sapper-fleet", "root": "runs"})
            };
            let target = args
                .target
                .as_deref()
                .ok_or_else(|| SapperError::new("fleet fire needs --target <challenge>"))?;
            let session = cfg["session"].as_str().unwrap_or("sapper-fleet");
            let killed = tmux::fire_worker(session, target).unwrap_or(false);
            println!(
                "[fleet] fire {target}: {}",
                if killed {
                    "released"
                } else {
                    "no worker running"
                }
            );
            Ok(0)
        }
        "workers" => {
            let cfg = if path.exists() {
                fleet_load(path)
                    .unwrap_or_else(|_| json!({"session": "sapper-fleet", "root": "runs"}))
            } else {
                json!({"session": "sapper-fleet", "root": "runs"})
            };
            let session = cfg["session"].as_str().unwrap_or("sapper-fleet");
            let workers = {
                let mut w: Vec<String> = tmux::active_workers(session)
                    .into_iter()
                    .map(|s| format!("w-{s}"))
                    .collect();
                w.sort();
                w
            };
            let workers_label = if workers.is_empty() {
                "(none)".to_string()
            } else {
                workers.join(", ")
            };
            println!("hired workers: {workers_label}");
            Ok(0)
        }
        "status" => {
            let cfg = fleet_load(path)?;
            let session = cfg["session"].as_str().unwrap_or("");
            if !tmux::tmux_has_session(session) {
                println!("session {session:?}: not running");
                return Ok(0);
            }
            let out = tmux::tmux(
                &[
                    "list-windows",
                    "-t",
                    session,
                    "-F",
                    "#I #W active=#{window_active} #{pane_current_command}",
                ],
                false,
            )?;
            println!(
                "session {session:?}:\n{}",
                String::from_utf8_lossy(&out.stdout).trim()
            );
            Ok(0)
        }
        other => Err(SapperError::new(format!("unknown fleet action {other:?}"))),
    }
}

pub struct FleetArgs {
    pub action: String,
    pub file: String,
    pub target: Option<String>,
    pub minutes: Option<i64>,
    pub max_steps: Option<i64>,
    pub extra_args: Option<String>,
}
