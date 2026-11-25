//! Field commander: LLM-driven cross-challenge allocation (port of
//! `cmd_command`). Layered control: fleet -> command -> auto.

use crate::dispatch::load_template;
use crate::engine::run_engine;
use crate::error::{Result, SapperError};
use crate::graph::{build_graph_view, extract_json_object, replace_placeholders};
use crate::store::Store;
use crate::tmux;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct CommandOpts {
    pub root: String,
    pub minutes: Option<i64>,
    pub deadline: Option<String>,
    pub minutes_default: i64,
    pub minutes_max: i64,
    pub fleet_file: Option<String>,
    pub poll_seconds: i64,
    pub model: String,
    pub timeout: u64,
    pub templates: Option<String>,
    pub skip_permissions: bool,
    pub verify: bool,
    pub max_steps: i64,
}

#[derive(Clone)]
pub struct BoardRow {
    pub name: String,
    pub status: String,
    pub goal: String,
    pub flags_submitted: i64,
    pub flags_recorded: i64,
    pub kinds: String,
    pub open_intents: i64,
    pub last_activity: String,
    pub worker: Option<String>,
}

fn board_summary(root: &Path) -> Vec<BoardRow> {
    let mut rows: Vec<BoardRow> = vec![];
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return rows,
    };
    for entry in entries.flatten() {
        let store_dir = entry.path().join(".sapper");
        if !store_dir.join("project.json").exists() {
            continue;
        }
        let store = Store::new(&store_dir);
        let g = match build_graph_view(&store) {
            Ok(g) => g,
            Err(_) => continue,
        };
        let mut kinds: HashMap<String, i64> = HashMap::new();
        let mut flags_submitted = 0;
        let mut flags_recorded = 0;
        let mut last_activity = String::new();
        for f in &g.facts {
            if let Some(k) = f.get("kind").and_then(|v| v.as_str()) {
                if !k.is_empty() {
                    *kinds.entry(k.to_string()).or_insert(0) += 1;
                }
            }
            if f.get("kind").and_then(|v| v.as_str()) == Some("flag") {
                let status = f
                    .get("data")
                    .and_then(|d| d.get("status"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("recorded");
                if status == "submitted" {
                    flags_submitted += 1;
                } else {
                    flags_recorded += 1;
                }
            }
            let ca = f.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
            if ca > last_activity.as_str() {
                last_activity = ca.to_string();
            }
        }
        let mut kind_strs: Vec<(String, i64)> = kinds.into_iter().collect();
        kind_strs.sort();
        rows.push(BoardRow {
            name: entry.file_name().to_string_lossy().to_string(),
            status: g.project["status"].as_str().unwrap_or("?").to_string(),
            goal: g
                .project
                .get("goal")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .chars()
                .take(80)
                .collect(),
            flags_submitted,
            flags_recorded,
            kinds: kind_strs
                .iter()
                .map(|(k, v)| format!("{v} {k}"))
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(40)
                .collect(),
            open_intents: g.open_intents.len() as i64,
            last_activity: if last_activity.is_empty() {
                "-".into()
            } else {
                last_activity
            },
            worker: None,
        });
    }
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows
}

fn render_board(rows: &[BoardRow]) -> String {
    let has_worker = rows.iter().any(|r| r.worker.is_some());
    let mut head = "challenge | status | flags(sub/rec) | intel | open | last activity".to_string();
    if has_worker {
        head.push_str(" | worker");
    }
    let mut lines = vec![format!("{head} | goal")];
    for r in rows {
        let mut line = format!(
            "{} | {} | {}/{} | {} | {} | {}",
            r.name,
            r.status,
            r.flags_submitted,
            r.flags_recorded,
            r.kinds,
            r.open_intents,
            r.last_activity
        );
        if has_worker {
            line.push_str(&format!(" | {}", r.worker.as_deref().unwrap_or("-")));
        }
        lines.push(format!("{line} | {}", r.goal));
    }
    lines.push(
        "note: flags(sub/rec) = submitted/recorded-but-not-submitted; intel = counts by kind (host service cred vuln loot flag); worker=yes means a hired tmux worker (w-<name>) is already on it."
            .to_string(),
    );
    lines.join("\n")
}

fn parse_command_payload(payload: &Value) -> Result<(String, String, i64, String)> {
    if payload.get("accepted") == Some(&Value::Bool(false)) {
        return Ok((
            "reject".into(),
            String::new(),
            0,
            payload
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        ));
    }
    let data: &Value = if payload.get("data").map(|v| v.is_object()).unwrap_or(false) {
        &payload["data"]
    } else {
        payload
    };
    if !data.is_object() {
        return Err(SapperError::new(format!(
            "invalid command payload: {}",
            serde_json::to_string(payload).unwrap_or_default()
        )));
    }
    let action = data
        .get("action")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SapperError::new("command payload action must be work|hire|fire|stop"))?;
    if !matches!(action, "work" | "hire" | "fire" | "stop") {
        return Err(SapperError::new(format!(
            "command payload action must be work|hire|fire|stop, got {action:?}"
        )));
    }
    if action == "stop" {
        return Ok((
            "stop".into(),
            String::new(),
            0,
            data.get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        ));
    }
    let target = data
        .get("target")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            SapperError::new(format!(
                "command payload {action} needs a 'target' challenge name"
            ))
        })?
        .trim()
        .to_string();
    if target.is_empty() {
        return Err(SapperError::new(format!(
            "command payload {action} needs a 'target' challenge name"
        )));
    }
    let mut minutes = 0;
    if matches!(action, "work" | "hire") {
        let m = data.get("minutes").cloned().unwrap_or(Value::from(20));
        let m: i64 = match &m {
            Value::Number(n) => n.as_i64().ok_or_else(|| {
                SapperError::new("command payload 'minutes' must be a positive number")
            })?,
            _ => {
                return Err(SapperError::new(
                    "command payload 'minutes' must be a positive number",
                ))
            }
        };
        if m < 1 {
            return Err(SapperError::new(
                "command payload 'minutes' must be a positive number",
            ));
        }
        minutes = m;
    }
    let note = data
        .get("note")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .chars()
        .take(300)
        .collect();
    Ok((action.to_string(), target, minutes, note))
}

fn command_fleet_cfg(args: &CommandOpts) -> HashMap<String, Value> {
    let path: PathBuf = match &args.fleet_file {
        Some(f) => PathBuf::from(f),
        None => PathBuf::from("fleet.json"),
    };
    let mut cfg: HashMap<String, Value> = if path.exists() {
        match crate::store::read_json(&path) {
            Ok(v) if v.get("session").and_then(|s| s.as_str()).is_some() => {
                let mut m: HashMap<String, Value> = v
                    .as_object()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .collect();
                m.entry("session".into())
                    .or_insert_with(|| Value::String("sapper-fleet".into()));
                m.entry("root".into())
                    .or_insert_with(|| Value::String(args.root.clone()));
                m
            }
            _ => HashMap::new(),
        }
    } else {
        HashMap::new()
    };
    cfg.entry("session".into())
        .or_insert_with(|| Value::String("sapper-fleet".into()));
    cfg.entry("root".into())
        .or_insert_with(|| Value::String(args.root.clone()));
    cfg
}

fn self_cli() -> Vec<String> {
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "sapper".to_string());
    vec![exe]
}

pub fn cmd_command(args: &CommandOpts) -> Result<i32> {
    let root = PathBuf::from(&args.root);
    if !root.is_dir() {
        return Err(SapperError::new(format!(
            "command root {} is not a directory",
            root.display()
        )));
    }
    let mut any_graph = false;
    if let Ok(entries) = std::fs::read_dir(&root) {
        for e in entries.flatten() {
            if e.path().join(".sapper").join("project.json").exists() {
                any_graph = true;
            }
        }
    }
    if !any_graph {
        return Err(SapperError::new(format!(
            "no */.sapper/project.json found under {}",
            root.display()
        )));
    }
    let deadline = crate::util::deadline_from_args(args.deadline.as_deref(), args.minutes)?;
    let Some(deadline) = deadline else {
        return Err(SapperError::new(
            "command needs --minutes or --deadline (the match slot clock)",
        ));
    };
    let templates_dir = crate::view::resolve_templates_dir(args.templates.as_deref(), &root);
    let cwd = std::env::current_dir()?;

    let mut passthrough: Vec<String> = vec!["--model".into(), args.model.clone()];
    if args.skip_permissions {
        passthrough.push("--skip-permissions".into());
    }
    if args.verify {
        passthrough.push("--verify".into());
    }
    if let Some(t) = &args.templates {
        passthrough.push("--templates".into());
        passthrough.push(t.clone());
    }

    let mut fleet_cfg = command_fleet_cfg(args);
    fleet_cfg.insert(
        "_base_dir".into(),
        Value::String(cwd.to_string_lossy().to_string()),
    );
    let session = fleet_cfg
        .get("session")
        .and_then(|v| v.as_str())
        .unwrap_or("sapper-fleet")
        .to_string();

    let deadline_str = crate::util::local_hhmm(deadline);
    println!(
        "[command] field commander over {}  deadline {deadline_str}",
        root.display()
    );
    println!("[command] fleet session {session:?} (hire/fire via tmux windows w-*)");
    let tpl = load_template(&templates_dir, "command.md")?;
    let mut noop_streak = 0;
    let mut worked: HashMap<String, i64> = HashMap::new();
    let now_epoch = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64()
    };

    let re = regex::Regex::new(r"\{([A-Za-z_][A-Za-z0-9_]*)\}").unwrap();

    while now_epoch() < deadline {
        let workers = tmux::active_workers(&session);
        let mut rows: Vec<BoardRow> = board_summary(&root)
            .into_iter()
            .filter(|r| r.status != "completed")
            .collect();
        for r in rows.iter_mut() {
            r.worker = Some(if workers.contains(&r.name) {
                "yes".to_string()
            } else {
                "-".to_string()
            });
        }
        if rows.is_empty() && workers.is_empty() {
            println!("[command] all challenge graphs completed");
            break;
        }
        if rows.is_empty() {
            let mut sorted_w: Vec<&String> = workers.iter().collect();
            sorted_w.sort();
            println!("[command] waiting on hired workers: {sorted_w:?}");
            std::thread::sleep(std::time::Duration::from_secs(
                (args.poll_seconds.max(0)) as u64,
            ));
            continue;
        }
        let remaining_min = ((deadline - now_epoch()) / 60.0) as i64;
        let remaining_min = remaining_min.max(1);

        let decision = (|| -> Result<(String, String, i64, String)> {
            let mut m: HashMap<String, String> = HashMap::new();
            m.insert("board".into(), render_board(&rows));
            m.insert(
                "time_budget".into(),
                crate::util::format_time_budget(Some(deadline)),
            );
            let prompt = re
                .replace_all(&tpl, |caps: &regex::Captures| {
                    m.get(&caps[1])
                        .cloned()
                        .unwrap_or_else(|| caps[0].to_string())
                })
                .to_string();
            let envelope = run_engine(&prompt, &args.model, args.timeout, false, None, &cwd)?;
            let text = envelope
                .get("result")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let payload = extract_json_object(text)?;
            let (action, target, minutes_raw, note) = parse_command_payload(&payload)?;
            Ok((action, target, minutes_raw, note))
        })();
        let (action, target, minutes, note) = match decision {
            Ok((a, t, m, n)) => (
                a,
                t,
                ((m.max(1)).min(args.minutes_max)).min(remaining_min),
                n,
            ),
            Err(exc) => {
                println!(
                    "[command] model decision failed ({}); falling back to round-robin",
                    exc.0
                );
                let free: Vec<&BoardRow> =
                    rows.iter().filter(|r| !workers.contains(&r.name)).collect();
                let min_worked = worked.values().min().cloned().unwrap_or(0);
                let row: &BoardRow = free
                    .iter()
                    .find(|r| worked.get(&r.name).cloned().unwrap_or(0) == min_worked)
                    .copied()
                    .or(rows.first())
                    .unwrap();
                (
                    "work".into(),
                    row.name.clone(),
                    args.minutes_default.min(remaining_min),
                    "round-robin fallback".into(),
                )
            }
        };
        if action == "reject" {
            noop_streak += 1;
            if noop_streak >= 2 {
                println!("[command] model rejected the board twice — stopping");
                break;
            }
            continue;
        }
        if action == "stop" {
            println!("[command] model says stop: {note}");
            break;
        }
        let names: std::collections::HashSet<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        if !names.contains(target.as_str()) {
            println!("[command] unknown/parked target {target:?}; skipping decision");
            noop_streak += 1;
            if noop_streak >= 3 {
                break;
            }
            continue;
        }
        noop_streak = 0;
        let prev = worked.get(&target).cloned().unwrap_or(0);
        worked.insert(target.clone(), prev + minutes);
        if action == "hire" {
            let session = fleet_cfg
                .get("session")
                .and_then(|v| v.as_str())
                .unwrap_or("sapper-fleet");
            let cfg_root = fleet_cfg
                .get("root")
                .and_then(|v| v.as_str())
                .unwrap_or("runs");
            let base_dir = PathBuf::from(
                fleet_cfg
                    .get("_base_dir")
                    .and_then(|v| v.as_str())
                    .unwrap_or("."),
            );
            let extra = passthrough[1..].join(" ");
            let engine_prefix = crate::tmux::engine_prefix();
            let spawned = tmux::hire_worker(
                session,
                cfg_root,
                &target,
                minutes,
                args.max_steps,
                &extra,
                &base_dir,
                &engine_prefix,
            )
            .unwrap_or(false);
            let tag = if spawned {
                "hired"
            } else {
                "already has a worker"
            };
            println!("[command] hire {target} for {minutes}m -> {tag} ({note})");
            std::thread::sleep(std::time::Duration::from_secs(
                (args.poll_seconds.max(0)) as u64,
            ));
            continue;
        }
        if action == "fire" {
            let session = fleet_cfg
                .get("session")
                .and_then(|v| v.as_str())
                .unwrap_or("sapper-fleet");
            let killed = tmux::fire_worker(session, &target).unwrap_or(false);
            println!(
                "[command] fire {target} -> {} ({note})",
                if killed { "released" } else { "no worker" }
            );
            std::thread::sleep(std::time::Duration::from_secs(
                (args.poll_seconds.max(0)) as u64,
            ));
            continue;
        }
        println!(
            "\n[command] -> {target} for {minutes}m ({note}) [{}]",
            crate::util::format_time_budget(Some(deadline))
        );
        let cli = self_cli();
        let mut cmd = vec![
            cli[0].clone(),
            "--store".into(),
            root.join(&target)
                .join(".sapper")
                .to_string_lossy()
                .to_string(),
            "auto".into(),
            "--minutes".into(),
            minutes.to_string(),
            "--max-steps".into(),
            args.max_steps.to_string(),
        ];
        cmd.extend(passthrough.iter().cloned());
        let _ = Command::new(&cmd[0])
            .args(&cmd[1..])
            .current_dir(root.join(&target))
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status();
    }

    println!("[command] final board:");
    for r in board_summary(&root) {
        let mark = if r.status == "completed" {
            "DONE"
        } else {
            &r.status
        };
        println!(
            "[command]   {}: {mark}  flags={}/{}",
            r.name, r.flags_submitted, r.flags_recorded
        );
    }
    Ok(0)
}

#[allow(dead_code)]
fn _unused_replace(_s: &str) {
    let _ = replace_placeholders;
}
