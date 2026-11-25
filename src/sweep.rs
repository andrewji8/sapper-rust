//! sweep — round-robin `auto` across per-challenge graphs under a runs/ root.
//! Port of `cmd_sweep`.

use crate::error::{Result, SapperError};
use crate::store::read_json;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct SweepOpts {
    pub root: String,
    pub minutes: Option<i64>,
    pub deadline: Option<String>,
    pub minutes_per: i64,
    pub model: String,
    pub templates: Option<String>,
    pub skip_permissions: bool,
    pub verify: bool,
    pub max_steps: i64,
}

fn self_cli() -> Vec<String> {
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "sapper".to_string());
    vec![exe]
}

fn graph_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = vec![];
    if let Ok(entries) = std::fs::read_dir(root) {
        for e in entries.flatten() {
            let p = e.path();
            if p.join(".sapper").join("project.json").exists() {
                dirs.push(p);
            }
        }
    }
    dirs.sort();
    dirs
}

pub fn cmd_sweep(o: &SweepOpts) -> Result<i32> {
    let root = PathBuf::from(&o.root);
    if !root.is_dir() {
        return Err(SapperError::new(format!(
            "sweep root {} is not a directory",
            root.display()
        )));
    }
    let graph_dirs = graph_dirs(&root);
    if graph_dirs.is_empty() {
        return Err(SapperError::new(format!(
            "no */.sapper/project.json found under {}",
            root.display()
        )));
    }
    let deadline = crate::util::deadline_from_args(o.deadline.as_deref(), o.minutes)?;
    let deadline = deadline.ok_or_else(|| {
        SapperError::new("sweep needs --minutes or --deadline (the match slot clock)")
    })?;
    if o.minutes_per < 1 {
        return Err(SapperError::new("--minutes-per must be at least 1"));
    }

    let cli = self_cli();
    let mut passthrough: Vec<String> = vec!["--model".into(), o.model.clone()];
    if o.skip_permissions {
        passthrough.push("--skip-permissions".into());
    }
    if o.verify {
        passthrough.push("--verify".into());
    }
    if let Some(t) = &o.templates {
        passthrough.push("--templates".into());
        passthrough.push(t.clone());
    }

    let now_epoch = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64()
    };

    let deadline_str = crate::util::local_hhmm(deadline);
    println!(
        "[sweep] {} challenge graph(s) under {}",
        graph_dirs.len(),
        root.display()
    );
    println!(
        "[sweep] deadline {deadline_str} ({})",
        crate::util::format_time_budget(Some(deadline))
    );

    while now_epoch() < deadline {
        let mut pending: Vec<PathBuf> = vec![];
        for d in &graph_dirs {
            let status = match read_json(&d.join(".sapper").join("project.json")) {
                Ok(v) => v
                    .get("status")
                    .and_then(|s| s.as_str())
                    .unwrap_or("active")
                    .to_string(),
                Err(_) => "active".into(),
            };
            if status != "completed" {
                pending.push(d.clone());
            }
        }
        if pending.is_empty() {
            println!("[sweep] all challenge graphs completed");
            break;
        }
        for d in &pending {
            let remaining = deadline - now_epoch();
            if remaining <= 0.0 {
                break;
            }
            let budget = (o.minutes_per).min(((remaining / 60.0) as i64).max(1));
            let _ = budget.max(1);
            println!(
                "\n[sweep] {}: auto for {}m ({} pending, {})",
                d.file_name().unwrap_or_default().to_string_lossy(),
                budget,
                pending.len(),
                crate::util::format_time_budget(Some(deadline))
            );
            let mut cmd: Vec<String> = vec![
                cli[0].clone(),
                "--store".into(),
                d.join(".sapper").to_string_lossy().to_string(),
                "auto".into(),
                "--minutes".into(),
                budget.to_string(),
                "--max-steps".into(),
                o.max_steps.to_string(),
            ];
            cmd.extend(passthrough.iter().cloned());
            let _ = Command::new(&cmd[0])
                .args(&cmd[1..])
                .current_dir(d)
                .stdin(Stdio::null())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status();
        }
    }

    let mut done = 0;
    for d in &graph_dirs {
        let status = match read_json(&d.join(".sapper").join("project.json")) {
            Ok(v) => v
                .get("status")
                .and_then(|s| s.as_str())
                .unwrap_or("?")
                .to_string(),
            Err(_) => "?".into(),
        };
        let mark = if status == "completed" {
            "DONE"
        } else {
            status.as_str()
        };
        println!(
            "[sweep] {}: {mark}",
            d.file_name().unwrap_or_default().to_string_lossy()
        );
        if status == "completed" {
            done += 1;
        }
    }
    println!(
        "[sweep] finished: {done}/{} goals reached",
        graph_dirs.len()
    );
    Ok(0)
}

#[allow(dead_code)]
fn _v(_: &Value) {}
