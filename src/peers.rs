//! tmux peer communication: discover / authorize / send / capture.
//! Port of `cmd_peers` / `cmd_send` / `cmd_capture` + peers config helpers.

use crate::error::{Result, SapperError};
use crate::store::Store;
use crate::tmux;
use crate::util::{
    dump_json, expand_user, is_peer_name, now_iso, now_local_stamp, AI_CHANNEL_NAME, PEERS_FILE,
};
use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;

fn peers_path(store: &Store) -> PathBuf {
    store.root.join(PEERS_FILE)
}

fn default_peers(store: &Store) -> Value {
    let channel = store.root.join(AI_CHANNEL_NAME);
    json!({
        "version": 1,
        "channel": channel.to_string_lossy(),
        "peers": {},
    })
}

pub fn load_peers(store: &Store) -> Result<Value> {
    let path = peers_path(store);
    if !path.exists() {
        return Ok(default_peers(store));
    }
    let data = crate::store::read_json(&path)
        .map_err(|e| SapperError::new(format!("cannot read peers config {path:?}: {}", e.0)))?;
    if !data.is_object() {
        return Err(SapperError::new(format!(
            "invalid peers config at {path:?}"
        )));
    }
    let mut data = data.clone();
    if data.get("version").map(|v| v.is_null()).unwrap_or(true) {
        data["version"] = json!(1);
    }
    let channel = store.root.join(AI_CHANNEL_NAME);
    if data
        .get("channel")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().is_empty())
        .unwrap_or(true)
    {
        data["channel"] = Value::String(channel.to_string_lossy().to_string());
    }
    if !data.get("peers").map(|v| v.is_object()).unwrap_or(false) {
        data["peers"] = json!({});
    }
    let version = data
        .get("version")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| {
            SapperError::new(format!(
                "invalid peers config at {path:?}: version must be an integer"
            ))
        })?;
    let _ = version;
    let channel = data
        .get("channel")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            SapperError::new(format!(
                "invalid peers config at {path:?}: channel must be a non-empty string"
            ))
        })?;
    if channel.trim().is_empty() {
        return Err(SapperError::new(format!(
            "invalid peers config at {path:?}: channel must be a non-empty string"
        )));
    }
    for (name, peer) in data["peers"].as_object().unwrap() {
        if !is_peer_name(name) {
            return Err(SapperError::new(format!(
                "invalid peers config at {path:?}: invalid peer name {name:?}"
            )));
        }
        if !peer.is_object() || peer.get("target").and_then(|v| v.as_str()).is_none() {
            return Err(SapperError::new(format!(
                "invalid peers config at {path:?}: peer {name:?} needs a target"
            )));
        }
    }
    Ok(data)
}

pub fn save_peers(store: &Store, data: &Value) -> Result<()> {
    let _lock = store.locked()?;
    crate::store::atomic_write_json(&peers_path(store), data)
}

// --------------------------------------------------------------------------- #
// peers subcommand
// --------------------------------------------------------------------------- #

pub fn cmd_peers(
    store: &Store,
    discover: bool,
    add: Option<&str>,
    remove: Option<&str>,
    target: Option<&str>,
    sender: Option<&str>,
    channel: Option<&str>,
    note: Option<&str>,
    as_json: bool,
) -> Result<i32> {
    if discover {
        let rows = tmux::tmux_discover()?;
        if as_json {
            let vals: Vec<Value> = rows
                .iter()
                .map(|r| {
                    json!({
                        "target": r.target,
                        "session": r.session,
                        "window": r.window,
                        "pane": r.pane,
                        "command": r.command,
                        "title": r.title,
                        "active": r.active,
                    })
                })
                .collect();
            println!("{}", dump_json(&Value::Array(vals)));
            return Ok(0);
        }
        if rows.is_empty() {
            println!("no tmux panes found");
            return Ok(0);
        }
        println!("tmux panes:");
        for r in &rows {
            let marker = if r.active { "*" } else { " " };
            let title = if r.title.is_empty() {
                String::new()
            } else {
                format!(" title={:?}", r.title)
            };
            println!(
                "  {marker} {:<14} session={:<10} window={:<12} cmd={}{}",
                r.target, r.session, r.window, r.command, title
            );
        }
        println!(
            "\nAuthorize explicitly before sending, for example:\n  sapper peers --add harley --target api-6:0.0 --sender \"Codex/api2-4\""
        );
        return Ok(0);
    }

    let mut data = load_peers(store)?;
    let peers = data["peers"].as_object().cloned().unwrap_or_default();

    if let Some(name) = add {
        let name = name.trim().to_string();
        if !is_peer_name(&name) {
            return Err(SapperError::new(
                "peer name must contain only letters, numbers, '.', '_' or '-'",
            ));
        }
        let target = target.ok_or_else(|| SapperError::new("--target is required with --add"))?;
        if !tmux::tmux_target_exists(target) {
            return Err(SapperError::new(format!("tmux target not found: {target}")));
        }
        if let Some(ch) = channel {
            let p = expand_user(ch);
            data["channel"] = Value::String(p.to_string_lossy().to_string());
        }
        let entry = json!({
            "target": target,
            "sender": sender
                .map(|s| s.to_string())
                .or_else(|| std::env::var("FGC_SENDER").ok())
                .unwrap_or_else(|| "sapper-agent".to_string()),
            "created_at": now_iso(),
            "note": note.map(|s| Value::String(s.to_string())).unwrap_or(Value::Null),
        });
        data["peers"]
            .as_object_mut()
            .unwrap()
            .insert(name.clone(), entry);
        save_peers(store, &data)?;
        println!(
            "authorized peer {name}: target={target} sender={}",
            data["peers"][&name]["sender"].as_str().unwrap_or("?")
        );
        return Ok(0);
    }

    if let Some(name) = remove {
        let name = name.trim().to_string();
        if !peers.contains_key(&name) {
            return Err(SapperError::new(format!("peer not configured: {name}")));
        }
        data["peers"].as_object_mut().unwrap().remove(&name);
        save_peers(store, &data)?;
        println!("removed peer {name}");
        return Ok(0);
    }

    if as_json {
        println!("{}", dump_json(&data));
        return Ok(0);
    }
    println!("peer config: {}", peers_path(store).display());
    println!("shared log:  {}", data["channel"].as_str().unwrap_or(""));
    if peers.is_empty() {
        println!("(no authorized peers)");
        println!("run `sapper peers --discover` to list tmux panes, then authorize one with --add");
        return Ok(0);
    }
    println!("authorized peers:");
    let mut names: Vec<&String> = peers.keys().collect();
    names.sort();
    for name in names {
        let peer = &peers[name];
        let note_s = peer
            .get("note")
            .and_then(|v| v.as_str())
            .map(|n| format!(" note={n:?}"))
            .unwrap_or_default();
        println!(
            "  {:<16} target={} sender={}{note_s}",
            name,
            peer["target"].as_str().unwrap_or("?"),
            peer["sender"].as_str().unwrap_or("?")
        );
    }
    Ok(0)
}

// --------------------------------------------------------------------------- #
// send subcommand
// --------------------------------------------------------------------------- #

pub fn cmd_send(
    store: &Store,
    peer: &str,
    message: &[String],
    sender: Option<&str>,
    channel: Option<&str>,
    no_log: bool,
    raw: bool,
) -> Result<i32> {
    let data = load_peers(store)?;
    let peers = data["peers"].as_object().unwrap();
    let peer_entry = peers.get(peer).ok_or_else(|| {
        SapperError::new(format!(
            "peer not configured: {peer}. Run `sapper peers` to see authorized peers."
        ))
    })?;
    let target = peer_entry
        .get("target")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SapperError::new(format!("peer {peer} has no tmux target")))?;
    if !tmux::tmux_target_exists(target) {
        return Err(SapperError::new(format!("tmux target not found: {target}")));
    }

    let body: String = if message.is_empty() {
        let mut stdin = String::new();
        use std::io::Read;
        std::io::stdin().read_to_string(&mut stdin)?;
        stdin.trim().to_string()
    } else {
        message.join(" ").trim().to_string()
    };
    if body.is_empty() {
        return Err(SapperError::new("empty message"));
    }

    let message = if raw {
        body
    } else {
        let stamp = now_local_stamp();
        let sender = sender
            .map(|s| s.to_string())
            .or_else(|| {
                peer_entry
                    .get("sender")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .or_else(|| std::env::var("FGC_SENDER").ok())
            .unwrap_or_else(|| "sapper-agent".to_string());
        format!("[{stamp}][{sender}] {body}")
    };

    if !no_log {
        let ch = PathBuf::from(
            channel
                .map(|c| c.to_string())
                .or_else(|| {
                    data.get("channel")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_else(|| {
                    store
                        .root
                        .join(AI_CHANNEL_NAME)
                        .to_string_lossy()
                        .to_string()
                }),
        );
        if let Some(parent) = ch.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut fh = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&ch)?;
        writeln!(fh, "{message}")?;
    }

    // Claude Code TUI often needs pasted LF, not tmux send-keys Enter. A
    // second LF makes submission reliable after idle / goal-complete states.
    tmux::tmux_paste(target, &message)?;
    std::thread::sleep(std::time::Duration::from_millis(env_float(
        "AI_SEND_INITIAL_DELAY",
        0.1,
    )));
    tmux::tmux_paste(target, "\n")?;
    std::thread::sleep(std::time::Duration::from_millis(env_float(
        "AI_SEND_SUBMIT_DELAY",
        0.5,
    )));
    tmux::tmux_paste(target, "\n")?;
    println!("sent to {peer} ({target}): {message}");
    Ok(0)
}

fn env_float(name: &str, default: f64) -> u64 {
    let v = std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(default);
    (v * 1000.0) as u64
}

// --------------------------------------------------------------------------- #
// capture subcommand
// --------------------------------------------------------------------------- #

pub fn cmd_capture(
    store: &Store,
    peer: &str,
    lines: i64,
    channel: Option<&str>,
    log: bool,
) -> Result<i32> {
    let data = load_peers(store)?;
    let peers = data["peers"].as_object().unwrap();
    let peer_entry = peers.get(peer).ok_or_else(|| {
        SapperError::new(format!(
            "peer not configured: {peer}. Run `sapper peers` to see authorized peers."
        ))
    })?;
    let target = peer_entry
        .get("target")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SapperError::new(format!("peer {peer} has no tmux target")))?;
    if !tmux::tmux_target_exists(target) {
        return Err(SapperError::new(format!("tmux target not found: {target}")));
    }
    if !(1..=2000).contains(&lines) {
        return Err(SapperError::new("--lines must be between 1 and 2000"));
    }

    let text = tmux::tmux_capture(target, lines)?;
    println!("{text}");

    if log {
        let ch = PathBuf::from(
            channel
                .map(|c| c.to_string())
                .or_else(|| {
                    data.get("channel")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_else(|| {
                    store
                        .root
                        .join(AI_CHANNEL_NAME)
                        .to_string_lossy()
                        .to_string()
                }),
        );
        if let Some(parent) = ch.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut fh = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&ch)?;
        let stamp = now_local_stamp();
        writeln!(
            fh,
            "[{stamp}][sapper-capture] peer={peer} target={target} lines={lines}"
        )?;
        writeln!(fh, "{text}")?;
    }
    Ok(0)
}
