//! tmux bridge (used by peers / fleet / command).
//!
//! On Windows, tmux is usually not available (WSL or an alternative like
//! `zellij`/`tmux` in a dev container is required). All tmux operations
//! degrade to a clear "tmux not available" error instead of panicking, so
//! the rest of the CLI (graph, engine, arena, kb, auto) works fully on
//! native Windows.

use crate::error::{Result, SapperError};
use serde_json::Value;
use std::process::{Command, Stdio};

fn tmux_run(args: &[&str]) -> std::result::Result<std::process::Output, SapperError> {
    Command::new("tmux")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| {
            SapperError::new(format!(
                "tmux not found on PATH ({}); peer/fleet tmux features need tmux — on Windows use WSL or a dev container",
                e
            ))
        })
}

/// Run a tmux command, returning the CompletedProcess equivalent.
pub fn tmux(args: &[&str], check: bool) -> Result<std::process::Output> {
    let out = tmux_run(args)?;
    if check && !out.status.success() {
        let err: String = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(SapperError::new(format!(
            "tmux {} failed: {}",
            args.first().copied().unwrap_or("?"),
            err.chars().take(200).collect::<String>()
        )));
    }
    Ok(out)
}

pub fn tmux_has_session(session: &str) -> bool {
    match tmux_run(&["has-session", "-t", session]) {
        Ok(o) => o.status.success(),
        Err(_) => false,
    }
}

pub fn tmux_target_exists(target: &str) -> bool {
    let out = Command::new("tmux")
        .args(["display-message", "-p", "-t", target, "#{pane_id}"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match out {
        Ok(mut c) => c.wait().map(|s| s.success()).unwrap_or(false),
        Err(_) => false,
    }
}

#[derive(Debug, Clone)]
pub struct TmuxPane {
    pub target: String,
    pub session: String,
    pub window: String,
    pub pane: String,
    pub command: String,
    pub title: String,
    pub active: bool,
}

pub fn tmux_discover() -> Result<Vec<TmuxPane>> {
    let fmt = "#{session_name}:#{window_index}.#{pane_index}\t#{session_name}\t#{window_name}\t#{pane_index}\t#{pane_current_command}\t#{pane_title}\t#{pane_active}";
    let out = match tmux_run(&["list-panes", "-a", "-F", fmt]) {
        Ok(o) => o,
        Err(_) => {
            return Err(SapperError::new(
                "tmux not found on PATH; peer discovery requires tmux (use WSL on Windows)",
            ))
        }
    };
    if !out.status.success() {
        return Err(SapperError::new(format!(
            "tmux list-panes failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let mut rows: Vec<TmuxPane> = vec![];
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() != 7 {
            continue;
        }
        rows.push(TmuxPane {
            target: parts[0].to_string(),
            session: parts[1].to_string(),
            window: parts[2].to_string(),
            pane: parts[3].to_string(),
            command: parts[4].to_string(),
            title: parts[5].to_string(),
            active: parts[6] == "1",
        });
    }
    Ok(rows)
}

pub fn tmux_paste(target: &str, text: &str) -> Result<()> {
    Command::new("tmux")
        .args(["load-buffer", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .and_then(|mut c| {
            if let Some(mut stdin) = c.stdin.take() {
                use std::io::Write;
                stdin.write_all(text.as_bytes())?;
            }
            let out = c.wait()?;
            if !out.success() {
                return Err(std::io::Error::other("tmux load-buffer failed"));
            }
            Ok(())
        })
        .map_err(|e| SapperError::new(format!("tmux load-buffer failed for {target}: {e}")))?;
    tmux(&["paste-buffer", "-t", target], true)?;
    Ok(())
}

pub fn tmux_capture(target: &str, lines: i64) -> Result<String> {
    let s = format!("-{lines}");
    let out = match tmux_run(&["capture-pane", "-p", "-J", "-t", target, "-S", &s]) {
        Ok(o) => o,
        Err(_) => {
            return Err(SapperError::new(
                "tmux not found on PATH; capture requires tmux (use WSL on Windows)",
            ))
        }
    };
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(SapperError::new(if err.is_empty() {
            format!("tmux capture failed for {target}")
        } else {
            err
        }));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// Challenge names with a live hired worker window (`w-<name>`).
pub fn active_workers(session: &str) -> std::collections::HashSet<String> {
    if !tmux_has_session(session) {
        return std::collections::HashSet::new();
    }
    let out = match tmux_run(&["list-windows", "-t", session, "-F", "#W"]) {
        Ok(o) if o.status.success() => o,
        _ => return std::collections::HashSet::new(),
    };
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .filter_map(|w| w.strip_prefix("w-").map(|s| s.to_string()))
        .collect()
}

/// Absolute, PATH-independent way to invoke this CLI in generated commands
/// (tmux panes may not have the sapper dir on PATH).
pub fn engine_prefix() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "sapper".to_string())
}

/// Spawn (or reuse) a tmux worker window w-<target> running `sapper auto`.
pub fn hire_worker(
    cfg_session: &str,
    cfg_root: &str,
    target: &str,
    minutes: i64,
    max_steps: i64,
    extra: &str,
    base_dir: &std::path::Path,
    engine_prefix: &str,
) -> Result<bool> {
    let window = format!("w-{target}");
    if active_workers(cfg_session).contains(&format!("w-{target}")) {
        return Ok(false);
    }
    if !tmux_has_session(cfg_session) {
        tmux(
            &["new-session", "-d", "-s", cfg_session, "-n", "commander"],
            true,
        )?;
    }
    let extra = extra.trim();
    let command = format!(
        "cd {} && {} --store {cfg_root}/{target}/.sapper auto --minutes {minutes} --max-steps {max_steps}{}",
        base_dir.display(),
        engine_prefix,
        if extra.is_empty() {
            String::new()
        } else {
            format!(" {extra}")
        }
    );
    tmux(
        &["new-window", "-d", "-t", cfg_session, "-n", &window],
        true,
    )?;
    // literal paste via buffer: avoids send-keys eating special chars
    let buf = format!(
        "{}.sapper-tmux-cmd-{}",
        cfg_session.replace('-', "_"),
        std::process::id()
    );
    let buf_path = std::path::PathBuf::from(&buf);
    std::fs::write(&buf_path, &command)?;
    tmux(
        &["load-buffer", buf_path.as_os_str().to_str().unwrap_or("")],
        true,
    )?;
    let tgt = format!("{cfg_session}:{window}");
    tmux(&["paste-buffer", "-t", &tgt], true)?;
    tmux(&["send-keys", "-t", &tgt, "Enter"], true)?;
    let _ = std::fs::remove_file(&buf_path);
    Ok(true)
}

pub fn fire_worker(session: &str, target: &str) -> Result<bool> {
    let window = format!("w-{target}");
    if !active_workers(session).contains(&window) {
        return Ok(false);
    }
    let t = format!("{session}:{window}");
    tmux(&["kill-window", "-t", &t], true)?;
    Ok(true)
}

#[allow(dead_code)]
fn _json(_v: &Value) {}
