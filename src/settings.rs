//! Idempotent add/remove of fact-graph hooks in a Claude Code
//! `settings.json` (project-level `<dir>/.claude/settings.json`).
//! Port of `_settings_patch.py`.

use crate::error::{Result, SapperError};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

const MARKER: &str = "sapper-hook";

// (hook event name, argv position passed to the hook)
const HOOK_EVENTS: [(&str, &str); 2] = [
    ("SessionStart", "sessionstart"),
    ("UserPromptSubmit", "userpromptsubmit"),
];

pub fn project_settings_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".claude").join("settings.json")
}

/// The command registered in settings.json points at the running sapper
/// binary's `hook` subcommand (self-contained, no external script path).
fn hook_command(arg: &str) -> String {
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "sapper".to_string());
    let exe = shell_quote(&exe);
    format!("{exe} hook {arg}")
}

/// Single-quote a Windows path so spaces/metachars don't break or inject.
fn shell_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '-' | '_'))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

fn load_settings(settings_path: &Path) -> Result<Value> {
    if !settings_path.exists() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    let raw = fs::read_to_string(settings_path)
        .map_err(|e| SapperError::new(format!("{settings_path:?} is not readable ({e})")))?;
    let raw = raw.trim();
    let data = if raw.is_empty() {
        Value::Object(serde_json::Map::new())
    } else {
        serde_json::from_str(raw)
            .map_err(|e| {
                SapperError::new(format!(
                    "{settings_path:?} is not valid JSON ({e}). Fix it by hand before re-running sapper setup."
                ))
            })?
    };
    if !data.is_object() {
        return Err(SapperError::new(format!(
            "{settings_path:?} must contain a JSON object at the top level"
        )));
    }
    Ok(data)
}

fn event_groups(
    hooks: &mut serde_json::Map<String, Value>,
    event: &str,
    create: bool,
) -> Result<Vec<Value>> {
    if !hooks.contains_key(event) {
        if !create {
            return Ok(vec![]);
        }
        hooks.insert(event.to_string(), Value::Array(vec![]));
    }
    let groups = hooks
        .get_mut(event)
        .ok_or_else(|| SapperError::new(format!("settings hooks.{event} missing")))
        .and_then(|v| {
            v.as_array_mut()
                .map(|a| a.clone())
                .ok_or_else(|| SapperError::new(format!("settings hooks.{event} must be a list")))
        })?;
    for (idx, group) in groups.iter().enumerate() {
        if !group.is_object() {
            return Err(SapperError::new(format!(
                "settings hooks.{event}[{idx}] must be an object"
            )));
        }
        let hook_list = group.get("hooks");
        if !hook_list.map(|v| v.is_array()).unwrap_or(false) {
            return Err(SapperError::new(format!(
                "settings hooks.{event}[{idx}].hooks must be a list"
            )));
        }
        if let Some(hl) = hook_list.and_then(|v| v.as_array()) {
            for (hidx, hook) in hl.iter().enumerate() {
                if !hook.is_object() {
                    return Err(SapperError::new(format!(
                        "settings hooks.{event}[{idx}].hooks[{hidx}] must be an object"
                    )));
                }
            }
        }
    }
    Ok(groups)
}

/// Write settings.json atomically so an interrupted patch cannot leave
/// truncated JSON.
fn write_settings(settings_path: &Path, data: &Value) -> Result<()> {
    if let Some(parent) = settings_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut s = serde_json::to_string_pretty(data).unwrap_or_default();
    s.push('\n');
    let tmp = settings_path.with_extension("json.tmp");
    fs::write(&tmp, &s)?;
    fs::rename(&tmp, settings_path)?;
    Ok(())
}

pub fn validate(settings_path: &Path) -> Result<Value> {
    let data = load_settings(settings_path)?;
    let hooks = data.get("hooks");
    if !hooks.map(|v| v.is_object()).unwrap_or(false) {
        if hooks.is_some() {
            return Err(SapperError::new("settings 'hooks' must be an object"));
        }
        return Ok(data);
    }
    let mut hooks_map = hooks.cloned().unwrap().as_object().unwrap().clone();
    for (event, _arg) in HOOK_EVENTS {
        let _ = event_groups(&mut hooks_map, event, false);
    }
    Ok(data)
}

fn has_hook(entries: &[Value], marker: &str) -> bool {
    for group in entries {
        let Some(hooks) = group.get("hooks").and_then(|v| v.as_array()) else {
            continue;
        };
        for h in hooks {
            let cmd = h.get("command").and_then(|v| v.as_str()).unwrap_or("");
            if cmd.contains(marker) {
                return true;
            }
        }
    }
    false
}

/// Register both hooks into settings_path. Returns (changed, messages).
pub fn add(settings_path: &Path, _hook_script: &str) -> Result<(bool, Vec<String>)> {
    let _ = settings_path
        .parent()
        .and_then(|p| fs::create_dir_all(p).ok());
    let mut data = load_settings(settings_path)?;
    if settings_path.exists() {
        let bak = settings_path.with_extension("json.bak");
        if !bak.exists() {
            fs::copy(settings_path, &bak)?;
        }
    }
    let mut hooks = data.get("hooks").cloned().unwrap_or(json!({}));
    if !hooks.is_object() {
        return Err(SapperError::new("settings 'hooks' must be an object"));
    }
    let mut changed = false;
    let mut msgs: Vec<String> = vec![];
    let hooks_obj = hooks.as_object_mut().unwrap();
    for (event, arg) in HOOK_EVENTS {
        let mut groups = event_groups(hooks_obj, event, true)?;
        if has_hook(&groups, MARKER) {
            msgs.push(format!("  = {event}: already present"));
            continue;
        }
        groups.push(json!({
            "matcher": "",
            "hooks": [{"type": "command", "command": hook_command(arg)}],
        }));
        hooks_obj.insert(event.to_string(), Value::Array(groups));
        changed = true;
        msgs.push(format!("  + {event}: {arg}"));
    }
    data["hooks"] = Value::Object(hooks_obj.clone());
    if changed {
        write_settings(settings_path, &data)?;
        msgs.push(format!("wrote {settings_path:?}"));
    } else {
        msgs.push(format!("idempotent — nothing changed in {settings_path:?}"));
    }
    Ok((changed, msgs))
}

/// Remove only our hook entries from settings_path.
pub fn remove(settings_path: &Path, _hook_script: &str) -> Result<(bool, Vec<String>)> {
    if !settings_path.exists() {
        return Ok((
            false,
            vec![format!("no {settings_path:?} — nothing to remove")],
        ));
    }
    let mut data = load_settings(settings_path)?;
    let mut hooks = data.get("hooks").cloned().unwrap_or(json!({}));
    if !hooks.is_object() {
        return Err(SapperError::new("settings 'hooks' must be an object"));
    }
    let mut changed = false;
    let mut msgs: Vec<String> = vec![];
    let hooks_obj = hooks.as_object_mut().unwrap();
    for (event, _arg) in HOOK_EVENTS {
        let groups = event_groups(hooks_obj, event, false)?;
        let mut new_groups: Vec<Value> = vec![];
        let mut removed_here = false;
        for group in groups {
            let mut group = group;
            let hs = group
                .get("hooks")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let kept: Vec<Value> = hs
                .iter()
                .filter(|h| {
                    let cmd = h.get("command").and_then(|v| v.as_str()).unwrap_or("");
                    !cmd.contains(MARKER)
                })
                .cloned()
                .collect();
            if kept.len() != hs.len() {
                changed = true;
                removed_here = true;
            }
            if !kept.is_empty() {
                group["hooks"] = Value::Array(kept);
                new_groups.push(group);
            }
        }
        hooks_obj.insert(event.to_string(), Value::Array(new_groups.clone()));
        if removed_here {
            msgs.push(format!("  - {event}: removed"));
        }
    }
    if changed {
        // tidy: drop now-empty hook-event lists and an all-empty hooks block
        let hooks_obj = hooks.as_object_mut().unwrap();
        for event in HOOK_EVENTS.iter().map(|e| e.0).collect::<Vec<_>>() {
            hooks_obj.remove(event);
        }
        // remove empty event lists
        let keys: Vec<String> = hooks_obj.keys().cloned().collect();
        for k in keys {
            let empty = hooks_obj
                .get(&k)
                .and_then(|v| v.as_array())
                .map(|a| a.is_empty())
                .unwrap_or(false);
            if empty {
                hooks_obj.remove(&k);
            }
        }
        if hooks_obj.is_empty() {
            data.as_object_mut().unwrap().remove("hooks");
        }
        write_settings(settings_path, &data)?;
        msgs.push(format!("wrote {settings_path:?}"));
    } else {
        msgs.push("no fact-graph hooks found to remove".to_string());
    }
    Ok((changed, msgs))
}
