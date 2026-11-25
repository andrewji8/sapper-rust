//! Graph helpers: view materialization, validation, frontier, recall,
//! prompt rendering (mirrors the Python renderers byte-for-byte where they
//! feed agent prompts).

use crate::error::{Result, SapperError};
use crate::store::Store;
use crate::util::{prompt_inline, read_json, scrub_inline};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

pub fn is_open(intent: &Value) -> bool {
    intent["to"].is_null()
}

/// Whether an intent can be worked safely. `fact_ids` optional.
pub fn is_ready(intent: &Value, fact_ids: Option<&std::collections::HashSet<String>>) -> bool {
    if !is_open(intent) {
        return false;
    }
    if let Some(ids) = fact_ids {
        let from: Vec<String> = intent["from"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        if !from.iter().all(|f| ids.contains(f)) {
            return false;
        }
    }
    let needs = intent
        .get("requires_confirmation")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let confirmed = intent
        .get("confirmed_at")
        .and_then(|v| v.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    if needs && !confirmed {
        return false;
    }
    true
}

pub struct GraphView {
    pub project: Value,
    pub facts: Vec<Value>,
    pub intents: Vec<Value>,
    pub open_intents: Vec<Value>,
    pub done_intents: Vec<Value>,
    pub hints: Vec<Value>,
    pub fact_ids: Vec<String>,
}

pub fn build_graph_view(store: &Store) -> Result<GraphView> {
    let facts = store.list_facts()?;
    let intents = store.list_intents()?;
    let hints = store.list_hints()?;
    let mut fact_ids: Vec<String> = facts
        .iter()
        .filter_map(|f| f["id"].as_str().map(|s| s.to_string()))
        .collect();
    fact_ids.sort();
    let open_intents: Vec<Value> = intents
        .iter()
        .filter(|i| i["to"].is_null())
        .cloned()
        .collect();
    let done_intents: Vec<Value> = intents
        .iter()
        .filter(|i| !i["to"].is_null())
        .cloned()
        .collect();
    let project = store.load_project()?;
    Ok(GraphView {
        project,
        facts,
        intents,
        open_intents,
        done_intents,
        hints,
        fact_ids,
    })
}

/// Serialize the view the way Python's `build_graph_view` dict serializes
/// (used for `graph --format json` and the view/JSON poll endpoint).
pub fn graph_view_json(view: &GraphView) -> Value {
    let mut m = serde_json::Map::new();
    m.insert("project".into(), view.project.clone());
    m.insert("facts".into(), Value::Array(view.facts.clone()));
    m.insert("intents".into(), Value::Array(view.intents.clone()));
    m.insert(
        "open_intents".into(),
        Value::Array(view.open_intents.clone()),
    );
    m.insert(
        "done_intents".into(),
        Value::Array(view.done_intents.clone()),
    );
    m.insert("hints".into(), Value::Array(view.hints.clone()));
    m.insert(
        "fact_ids".into(),
        Value::Array(
            view.fact_ids
                .iter()
                .map(|s| Value::String(s.clone()))
                .collect(),
        ),
    );
    Value::Object(m)
}

/// Return integrity errors without mutating the graph.
pub fn validate_graph(store: &Store) -> Vec<String> {
    let mut errors: Vec<String> = vec![];
    let project = match store.load_project() {
        Ok(p) => p,
        Err(e) => return vec![e.0],
    };
    let status = project["status"].as_str().unwrap_or("");
    if status != "active" && status != "completed" {
        errors.push(format!(
            "project status must be 'active' or 'completed', got {status:?}"
        ));
    }
    let counters = project.get("counters").cloned().unwrap_or(Value::Null);
    for kind in ["fact", "intent", "hint", "run"] {
        if !counters.get(kind).map(|_| true).unwrap_or(false) {
            errors.push(format!("project counters missing {kind:?}"));
        }
    }

    let mut by_kind: HashMap<String, HashMap<String, Value>> = HashMap::new();
    for (kind, directory) in [
        ("fact", &store.facts_dir),
        ("intent", &store.intents_dir),
        ("hint", &store.hints_dir),
    ] {
        let table = by_kind.entry(kind.to_string()).or_default();
        let mut paths: Vec<std::path::PathBuf> = if directory.is_dir() {
            std::fs::read_dir(directory)
                .map(|it| {
                    it.flatten()
                        .map(|e| e.path())
                        .filter(|p| {
                            p.extension()
                                .map(|e| e.to_string_lossy() == "json")
                                .unwrap_or(false)
                        })
                        .collect()
                })
                .unwrap_or_default()
        } else {
            vec![]
        };
        paths.sort();
        for path in paths {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let data = match read_json(&path) {
                Ok(d) => d,
                Err(e) => {
                    errors.push(format!("{kind} {name}: unreadable JSON ({})", e.0));
                    continue;
                }
            };
            let schema = crate::store::node_schema_errors(&data, kind);
            if !schema.is_empty() {
                errors.push(format!("{kind} {name}: {}", schema.join("; ")));
                continue;
            }
            let node_id = data["id"].as_str().unwrap_or("").to_string();
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if stem != node_id {
                errors.push(format!(
                    "{kind} {name}: filename does not match id {node_id:?}"
                ));
            }
            if table.contains_key(&node_id) {
                errors.push(format!("duplicate {kind} id {node_id:?}"));
            }
            table.insert(node_id, data);
        }
    }

    let facts = by_kind.get("fact").cloned().unwrap_or_default();
    let intents = by_kind.get("intent").cloned().unwrap_or_default();
    if !facts.contains_key("goal") {
        errors.push("missing terminal goal fact".into());
    } else if facts.get("goal").and_then(|g| g["description"].as_str())
        != project.get("goal").and_then(|g| g.as_str())
    {
        errors.push("goal fact description does not match project goal".into());
    }
    for (iid, intent) in &intents {
        if let Some(from_arr) = intent["from"].as_array() {
            for fid in from_arr.iter().filter_map(|v| v.as_str()) {
                if !facts.contains_key(fid) {
                    errors.push(format!("intent {iid}: missing dependency fact {fid}"));
                } else if fid == "goal" {
                    errors.push(format!("intent {iid}: goal cannot be a dependency"));
                }
            }
        }
        let target = intent["to"].as_str();
        if let Some(target) = target {
            if target != "goal" {
                match facts.get(target) {
                    Some(f) => {
                        let src = f
                            .get("source_intent")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if src != iid {
                            errors.push(format!(
                                "intent {iid}: target fact {target} does not name it as source intent"
                            ));
                        }
                    }
                    None => {
                        errors.push(format!("intent {iid}: target fact {target} does not exist"))
                    }
                }
            }
        }
    }
    for (fid, fact) in &facts {
        let source = fact.get("source_intent").and_then(|v| v.as_str());
        if let Some(source) = source {
            if !intents.contains_key(source) {
                errors.push(format!("fact {fid}: source intent {source} does not exist"));
            } else {
                let to = intents
                    .get(source)
                    .and_then(|i| i["to"].as_str())
                    .unwrap_or("");
                if to != fid {
                    errors.push(format!(
                        "fact {fid}: source intent {source} does not point to this fact"
                    ));
                }
            }
        }
    }
    let goal_intents: Vec<&String> = intents
        .iter()
        .filter(|(_, i)| i["to"].as_str() == Some("goal"))
        .map(|(k, _)| k)
        .collect();
    if status == "completed" && goal_intents.is_empty() {
        errors.push("project is completed but no intent targets the goal".into());
    }
    if status == "active" && !goal_intents.is_empty() {
        errors.push("project is active although an intent already targets the goal".into());
    }
    errors
}

pub fn frontier(store: &Store) -> Result<Vec<Value>> {
    let intents = store.list_intents()?;
    let fact_ids: std::collections::HashSet<String> = store
        .list_facts()?
        .iter()
        .filter_map(|f| f["id"].as_str().map(|s| s.to_string()))
        .collect();
    let mut ready: Vec<Value> = intents
        .into_iter()
        .filter(|i| is_ready(i, Some(&fact_ids)))
        .collect();
    ready.sort_by(|a, b| {
        let a_claimed = a["status"].as_str() == Some("claimed");
        let b_claimed = b["status"].as_str() == Some("claimed");
        // claimed first, then by created_at
        b_claimed.cmp(&a_claimed).then_with(|| {
            a["created_at"]
                .as_str()
                .unwrap_or("")
                .cmp(b["created_at"].as_str().unwrap_or(""))
        })
    });
    Ok(ready)
}

fn trunc_chars(s: &str, n: usize) -> usize {
    s.char_indices().nth(n).map(|(i, _)| i).unwrap_or(s.len())
}

pub fn render_status(store: &Store) -> Result<String> {
    let g = build_graph_view(store)?;
    let p = &g.project;
    let mut lines: Vec<String> = vec![];
    lines.push(format!(
        "project: {}  status: {}",
        scrub_inline(p.get("id")),
        scrub_inline(p.get("status"))
    ));
    lines.push(format!("origin: {}", scrub_inline(p.get("origin"))));
    lines.push(format!("goal:   {}", scrub_inline(p.get("goal"))));
    lines.push(format!(
        "counts: {} facts, {} open / {} done intents, {} hints",
        g.facts.len(),
        g.open_intents.len(),
        g.done_intents.len(),
        g.hints.len()
    ));
    let mut typed: HashMap<String, i64> = HashMap::new();
    for f in &g.facts {
        if let Some(k) = f.get("kind").and_then(|v| v.as_str()) {
            if !k.is_empty() {
                *typed.entry(k.to_string()).or_insert(0) += 1;
            }
        }
    }
    if !typed.is_empty() {
        let summary: Vec<String> = typed
            .iter()
            .map(|(k, n)| {
                if k != "flag" {
                    format!("{n} {k}s")
                } else {
                    format!("{n} flag(s)")
                }
            })
            .collect();
        lines.push(format!("kinds:  {}", summary.join(", ")));
    }
    let flags: Vec<&Value> = g
        .facts
        .iter()
        .filter(|f| f["kind"].as_str() == Some("flag"))
        .collect();
    for f in flags {
        let d = f.get("data").cloned().unwrap_or(Value::Null);
        let d: &Value = if d.is_object() { &d } else { &Value::Null };
        let ch = d
            .get("challenge")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let status = d
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("recorded");
        let val = scrub_inline(d.get("value"));
        let val = &val[..trunc_chars(&val, 48)];
        lines.push(format!(
            "flag:   {} status={}{} value={val}",
            f["id"].as_str().unwrap_or("?"),
            status,
            ch.map(|c| format!(" challenge={c}")).unwrap_or_default()
        ));
    }
    let fr = frontier(store)?;
    if !fr.is_empty() {
        lines.push("".into());
        lines.push("frontier (ready to work):".into());
        for i in &fr {
            let mut flag = String::new();
            let needs = i
                .get("requires_confirmation")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let confirmed = i
                .get("confirmed_at")
                .and_then(|v| v.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false);
            if needs && !confirmed {
                flag = "  [needs confirmation]".to_string();
            } else if i["status"].as_str() == Some("claimed") {
                flag = format!("  [claimed by {}]", i["worker"].as_str().unwrap_or("?"));
            }
            let from: Vec<&str> = i["from"]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            let desc = scrub_inline(i.get("description"));
            let desc = &desc[..trunc_chars(&desc, 80)];
            lines.push(format!(
                "  {}  from {from:?}  {desc}{flag}",
                i["id"].as_str().unwrap_or("?")
            ));
        }
    } else if !g.open_intents.is_empty() {
        lines.push("".into());
        lines.push("(open intents exist but none ready — check confirmations)".into());
    } else {
        lines.push("".into());
        lines.push("(no open intents — dispatch a reason step or complete)".into());
    }
    if !g.hints.is_empty() {
        lines.push("".into());
        lines.push(format!("hints: {} (use `sapper hint list`)", g.hints.len()));
    }
    Ok(lines.join("\n"))
}

// --------------------------------------------------------------------------- #
// recall
// --------------------------------------------------------------------------- #

pub fn recall_tokenize(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let mut out: Vec<String> = vec![];
    let mut word: Vec<char> = vec![];
    for c in lower.chars() {
        if c.is_alphanumeric() || c == '_' {
            word.push(c);
        } else {
            flush_word(&mut word, &mut out);
        }
    }
    flush_word(&mut word, &mut out);
    out.into_iter().filter(|t| !t.is_empty()).collect()
}

fn flush_word(word: &mut Vec<char>, out: &mut Vec<String>) {
    if word.is_empty() {
        return;
    }
    let s: String = word.drain(..).collect();
    let has_cjk = s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c));
    if has_cjk {
        for c in s.chars() {
            out.push(c.to_string());
        }
    } else {
        out.push(s);
    }
}

/// Port of `_recall_score`: token overlap (title x2) + small difflib bonus.
fn recall_score(query: &str, node: &Value) -> f64 {
    let title = node.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let desc = node
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let haystack = format!("{title} {title} {desc}").trim().to_string();
    if haystack.is_empty() {
        return 0.0;
    }
    let q_tokens: std::collections::HashSet<String> = recall_tokenize(query).into_iter().collect();
    if q_tokens.is_empty() {
        return 0.0;
    }
    let h_tokens: std::collections::HashSet<String> =
        recall_tokenize(&haystack).into_iter().collect();
    if h_tokens.is_empty() {
        return 0.0;
    }
    let overlap = q_tokens.iter().filter(|t| h_tokens.contains(*t)).count();
    let q_lower = query.to_lowercase();
    if overlap == 0 {
        let ratio = difflib_ratio(&q_lower, &haystack);
        return if ratio >= 0.45 { ratio } else { 0.0 };
    }
    let overlap_score = overlap as f64 / q_tokens.len().max(1) as f64;
    let ratio_bonus = 0.2 * difflib_ratio(&q_lower, &haystack);
    overlap_score + ratio_bonus
}

/// difflib.SequenceMatcher(None, a, b).ratio() = 2*M/T over the two strings.
fn difflib_ratio(a: &str, b: &str) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev = vec![0usize; b.len() + 1];
    let mut cur = prev.clone();
    for ca in a.iter() {
        std::mem::swap(&mut prev, &mut cur);
        cur[0] = 0;
        for j in 1..=b.len() {
            if ca == &b[j - 1] {
                cur[j] = prev[j - 1] + 1;
            } else {
                cur[j] = prev[j].max(cur[j - 1]);
            }
        }
    }
    let m = cur[b.len()];
    2.0 * m as f64 / (a.len() + b.len()) as f64
}

pub struct RecallHit {
    pub id: String,
    pub kind: &'static str,
    pub title: Option<String>,
    pub description: String,
    pub has_doc: bool,
    pub score: f64,
}

pub fn recall(store: &Store, query: &str, limit: usize, kinds: &[&str]) -> Result<Vec<RecallHit>> {
    if query.trim().is_empty() {
        return Ok(vec![]);
    }
    let mut candidates: Vec<RecallHit> = vec![];
    if kinds.contains(&"fact") {
        for f in store.list_facts()? {
            if f["id"].as_str() == Some("goal") {
                continue;
            }
            let s = recall_score(query, &f);
            if s > 0.0 {
                candidates.push(RecallHit {
                    id: f["id"].as_str().unwrap_or("?").to_string(),
                    kind: "fact",
                    title: f
                        .get("title")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    description: f
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    has_doc: f.get("doc").map(|v| !v.is_null()).unwrap_or(false),
                    score: s,
                });
            }
        }
    }
    if kinds.contains(&"intent") {
        for i in store.list_intents()? {
            let s = recall_score(query, &i);
            if s > 0.0 {
                candidates.push(RecallHit {
                    id: i["id"].as_str().unwrap_or("?").to_string(),
                    kind: "intent",
                    title: i
                        .get("title")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    description: i
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    has_doc: i.get("doc").map(|v| !v.is_null()).unwrap_or(false),
                    score: s,
                });
            }
        }
    }
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
    candidates.truncate(limit);
    Ok(candidates)
}

pub fn render_recall(matches: &[RecallHit], query: &str) -> String {
    if matches.is_empty() {
        return format!("(no similar prior work for {query:?})");
    }
    let mut lines: Vec<String> = vec![
        format!("# recall for: {query}"),
        "# use `sapper doc <id>` to read the detailed document".into(),
        String::new(),
    ];
    for m in matches {
        let title = m
            .title
            .as_deref()
            .map(|t| format!("[{}]  ", scrub_inline(Some(&Value::String(t.to_string())))))
            .unwrap_or_default();
        let doc = if m.has_doc { "  📄" } else { "" };
        let desc = scrub_inline(Some(&Value::String(m.description.clone())));
        let desc = &desc[..trunc_chars(&desc, 90)];
        lines.push(format!(
            "  {} ({}, score={:.2})  {title}{desc}{doc}",
            m.id, m.kind, m.score
        ));
    }
    lines.join("\n")
}

pub fn render_history_block(store: &Store, max_facts: usize, max_intents: usize) -> Result<String> {
    let facts: Vec<Value> = store
        .list_facts()?
        .into_iter()
        .filter(|f| f["id"].as_str() != Some("goal"))
        .collect();
    let mut facts = facts;
    facts.sort_by(|a, b| {
        a.get("created_at")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .cmp(b.get("created_at").and_then(|v| v.as_str()).unwrap_or(""))
            .reverse()
    });
    let mut done: Vec<Value> = store
        .list_intents()?
        .into_iter()
        .filter(|i| !i["to"].is_null())
        .collect();
    done.sort_by(|a, b| {
        a.get("concluded_at")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .cmp(b.get("concluded_at").and_then(|v| v.as_str()).unwrap_or(""))
            .reverse()
    });

    let mut lines: Vec<String> = vec![];
    if !facts.is_empty() {
        lines.push("history facts (most recent first):".into());
        for f in facts.iter().take(max_facts) {
            let title = f
                .get("title")
                .and_then(|v| v.as_str())
                .map(|s| format!("[{}] ", scrub_inline(Some(&Value::String(s.to_string())))))
                .unwrap_or_default();
            let doc = if f.get("doc").map(|v| !v.is_null()).unwrap_or(false) {
                " 📄"
            } else {
                ""
            };
            let desc = scrub_inline(f.get("description"));
            let desc = &desc[..trunc_chars(&desc, 90)];
            lines.push(format!(
                "  {}: {title}{desc}{doc}",
                f["id"].as_str().unwrap_or("?")
            ));
        }
    }
    if !done.is_empty() {
        if !lines.is_empty() {
            lines.push("".into());
        }
        lines.push("recently-done intents:".into());
        for i in done.iter().take(max_intents) {
            let title = i
                .get("title")
                .and_then(|v| v.as_str())
                .map(|s| format!("[{}] ", scrub_inline(Some(&Value::String(s.to_string())))))
                .unwrap_or_default();
            let doc = if i.get("doc").map(|v| !v.is_null()).unwrap_or(false) {
                " 📄"
            } else {
                ""
            };
            let desc = scrub_inline(i.get("description"));
            let desc = &desc[..trunc_chars(&desc, 90)];
            lines.push(format!(
                "  {}: {title}{desc}{doc}",
                i["id"].as_str().unwrap_or("?")
            ));
        }
    }
    if lines.is_empty() {
        Ok(String::new())
    } else {
        Ok(lines.join("\n"))
    }
}

pub fn render_graph_text(store: &Store) -> Result<String> {
    let g = build_graph_view(store)?;
    let mut lines: Vec<String> = vec![format!(
        "# graph  ({})",
        scrub_inline(g.project.get("status"))
    )];
    lines.push(String::new());
    lines.push("## facts".into());
    for f in &g.facts {
        let src = f
            .get("source_intent")
            .and_then(|v| v.as_str())
            .map(|s| format!("  <- {}", scrub_inline(Some(&Value::String(s.to_string())))))
            .unwrap_or_default();
        let t = f
            .get("title")
            .and_then(|v| v.as_str())
            .map(|s| format!("[{}]  ", scrub_inline(Some(&Value::String(s.to_string())))))
            .unwrap_or_default();
        let k = f
            .get("kind")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| format!("<{s}>"))
            .unwrap_or_default();
        let data = if f.get("data").map(|v| v.is_object()).unwrap_or(false) {
            let pairs: Vec<String> = f["data"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(key, val)| format!("{key}={}", scrub_inline(Some(val))))
                .collect();
            format!("  ({})", pairs.join(" "))
        } else {
            String::new()
        };
        let doc = if f.get("doc").map(|v| !v.is_null()).unwrap_or(false) {
            "  📄"
        } else {
            ""
        };
        lines.push(format!(
            "  {}: {k}{t}{}{data}{src}{doc}",
            f["id"].as_str().unwrap_or("?"),
            scrub_inline(f.get("description"))
        ));
    }
    lines.push(String::new());
    lines.push("## intents".into());
    for i in &g.intents {
        let to = i["to"].as_str().unwrap_or("·");
        let mut flag = String::new();
        let needs = i
            .get("requires_confirmation")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let confirmed = i
            .get("confirmed_at")
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        if needs && !confirmed {
            flag = "  !confirm".to_string();
        } else if i["status"].as_str() == Some("claimed") {
            flag = format!("  ~{}", i["worker"].as_str().unwrap_or("?"));
        }
        let t = i
            .get("title")
            .and_then(|v| v.as_str())
            .map(|s| format!("[{}]  ", scrub_inline(Some(&Value::String(s.to_string())))))
            .unwrap_or_default();
        let doc = if i.get("doc").map(|v| !v.is_null()).unwrap_or(false) {
            "  📄"
        } else {
            ""
        };
        let from: Vec<&str> = i["from"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        let desc = scrub_inline(i.get("description"));
        let desc = &desc[..trunc_chars(&desc, 70)];
        lines.push(format!(
            "  {}: {from:?} -> {to}  {t}{desc}{flag}{doc}",
            i["id"].as_str().unwrap_or("?")
        ));
    }
    if !g.hints.is_empty() {
        lines.push(String::new());
        lines.push("## hints".into());
        for h in &g.hints {
            lines.push(format!(
                "  {} ({}): {}",
                h["id"].as_str().unwrap_or("?"),
                scrub_inline(h.get("creator")),
                scrub_inline(h.get("content"))
            ));
        }
    }
    Ok(lines.join("\n"))
}

// --------------------------------------------------------------------------- #
// prompt rendering
// --------------------------------------------------------------------------- #

fn intent_brief(i: &Value) -> Value {
    let confirmed = i
        .get("confirmed_at")
        .and_then(|v| v.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    serde_json::json!({
        "id": i["id"],
        "from": i["from"],
        "description": i["description"],
        "requires_confirmation": i["requires_confirmation"],
        "confirmed": confirmed,
    })
}

fn hint_brief(h: &Value) -> Value {
    serde_json::json!({
        "id": h["id"],
        "creator": h["creator"],
        "content": h["content"],
    })
}

pub fn to_yaml_like(g: &GraphView) -> String {
    let mut lines: Vec<String> = vec![];
    lines.push(format!(
        "project: status={}",
        prompt_inline(g.project.get("status"))
    ));
    lines.push(format!("goal: {}", prompt_inline(g.project.get("goal"))));
    lines.push("facts:".into());
    for f in &g.facts {
        let src = f
            .get("source_intent")
            .and_then(|v| v.as_str())
            .map(|s| {
                format!(
                    " (from {})",
                    prompt_inline(Some(&Value::String(s.to_string())))
                )
            })
            .unwrap_or_default();
        let title = f
            .get("title")
            .and_then(|v| v.as_str())
            .map(|s| format!("[{}] ", prompt_inline(Some(&Value::String(s.to_string())))))
            .unwrap_or_default();
        let kind = f
            .get("kind")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| format!("<{s}> "))
            .unwrap_or_default();
        let data = if f.get("data").map(|v| v.is_object()).unwrap_or(false) {
            let pairs: Vec<String> = f["data"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| format!("{k}={}", prompt_inline(Some(v))))
                .collect();
            format!(" {{{}}}", pairs.join(" "))
        } else {
            String::new()
        };
        let has_doc = if f.get("doc").map(|v| !v.is_null()).unwrap_or(false) {
            " (has doc)"
        } else {
            ""
        };
        lines.push(format!(
            "  - {}: {kind}{title}{}{data}{src}{has_doc}",
            f["id"].as_str().unwrap_or("?"),
            prompt_inline(f.get("description"))
        ));
    }
    lines.push("intents:".into());
    for i in &g.intents {
        let to = i["to"].as_str().unwrap_or("open");
        let mut flag = String::new();
        let needs = i
            .get("requires_confirmation")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let confirmed = i
            .get("confirmed_at")
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        if needs && !confirmed {
            flag = " [needs_confirmation]".to_string();
        }
        let title = i
            .get("title")
            .and_then(|v| v.as_str())
            .map(|s| format!("[{}] ", prompt_inline(Some(&Value::String(s.to_string())))))
            .unwrap_or_default();
        let has_doc = if i.get("doc").map(|v| !v.is_null()).unwrap_or(false) {
            " (has doc)"
        } else {
            ""
        };
        let from: Vec<&str> = i["from"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        lines.push(format!(
            "  - {}: {from:?} -> {to}{flag} | {title}{}{has_doc}",
            i["id"].as_str().unwrap_or("?"),
            prompt_inline(i.get("description"))
        ));
    }
    if !g.hints.is_empty() {
        lines.push("hints:".into());
        for h in &g.hints {
            lines.push(format!(
                "  - {} ({}): {}",
                h["id"].as_str().unwrap_or("?"),
                prompt_inline(h.get("creator")),
                prompt_inline(h.get("content"))
            ));
        }
    }
    lines.push(
        "note: nodes marked (has doc) carry a detailed layer — read it with `sapper doc <id>` when you need the full context.".into(),
    );
    lines.join("\n")
}

pub fn render_prompt(store: &Store, template_text: &str, time_budget: &str) -> Result<String> {
    let g = build_graph_view(store)?;
    let p = &g.project;
    let mut replacements: HashMap<String, String> = HashMap::new();
    replacements.insert("origin".into(), prompt_inline(p.get("origin")));
    replacements.insert("goal".into(), prompt_inline(p.get("goal")));
    replacements.insert("graph_yaml".into(), to_yaml_like(&g));
    replacements.insert(
        "fact_ids".into(),
        serde_json::to_string_pretty(
            &g.fact_ids
                .iter()
                .map(|s| Value::String(s.clone()))
                .collect::<Vec<_>>(),
        )
        .unwrap_or_default(),
    );
    replacements.insert(
        "open_intents".into(),
        serde_json::to_string_pretty(&g.open_intents.iter().map(intent_brief).collect::<Vec<_>>())
            .unwrap_or_default(),
    );
    replacements.insert(
        "hints".into(),
        serde_json::to_string_pretty(&g.hints.iter().map(hint_brief).collect::<Vec<_>>())
            .unwrap_or_default(),
    );
    replacements.insert("max_intents".into(), "3".to_string());
    replacements.insert(
        "time_budget".into(),
        prompt_inline(Some(&Value::String(time_budget.to_string()))),
    );
    Ok(replace_placeholders(template_text, &replacements))
}

pub fn render_intent_prompt(store: &Store, template_text: &str, intent: &Value) -> Result<String> {
    let desc = intent
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let matches: Vec<RecallHit> = recall(store, desc, 6, &["fact", "intent"])?
        .into_iter()
        .filter(|m| m.id != intent["id"].as_str().unwrap_or(""))
        .take(5)
        .collect();
    let recall_block = if matches.is_empty() {
        "(no prior nodes look similar to this intent)".to_string()
    } else {
        render_recall(&matches, desc)
    };
    let g = build_graph_view(store)?;
    let mut replacements: HashMap<String, String> = HashMap::new();
    replacements.insert("graph_yaml".into(), to_yaml_like(&g));
    replacements.insert(
        "intent_id".into(),
        intent["id"].as_str().unwrap_or("").to_string(),
    );
    replacements.insert(
        "intent_description".into(),
        prompt_inline(intent.get("description")),
    );
    replacements.insert("recall_block".into(), recall_block);
    Ok(replace_placeholders(template_text, &replacements))
}

/// Single-pass placeholder substitution: only tokens present in the template
/// are rewritten; inserted graph content is never re-scanned.
pub fn replace_placeholders(template_text: &str, replacements: &HashMap<String, String>) -> String {
    let re = regex::Regex::new(r"\{([A-Za-z_][A-Za-z0-9_]*)\}").unwrap();
    re.replace_all(template_text, |caps: &regex::Captures| {
        let key = &caps[1];
        replacements
            .get(key)
            .cloned()
            .unwrap_or_else(|| caps[0].to_string())
    })
    .into_owned()
}

// --------------------------------------------------------------------------- #
// JSON extraction from LLM output
// --------------------------------------------------------------------------- #

fn extract_first_balanced(text: &str, start: usize) -> (Option<Value>, usize) {
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    for (i, ch) in text[start..].char_indices() {
        let i = start + i;
        if in_str {
            if esc {
                esc = false;
            } else if ch == '\\' {
                esc = true;
            } else if ch == '"' {
                in_str = false;
            }
        } else {
            match ch {
                '"' => in_str = true,
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        let blob = &text[start..i + 1];
                        return (serde_json::from_str(blob).ok(), i + 1);
                    }
                }
                _ => {}
            }
        }
    }
    (None, text.len())
}

pub fn extract_json_object(text: &str) -> Result<Value> {
    if text.trim().is_empty() {
        return Err(SapperError::new("empty model output"));
    }
    let mut pos = 0;
    let mut last_error: Option<String> = None;
    while pos <= text.len() {
        let Some(rel) = text[pos..].find('{') else {
            break;
        };
        let start = pos + rel;
        let (obj, end) = extract_first_balanced(text, start);
        if let Some(obj) = obj {
            return Ok(obj);
        }
        let blob_end = end.min(text.len());
        last_error = Some(text[start..blob_end].chars().take(200).collect());
        pos = if end > start { end } else { start + 1 };
    }
    if let Some(e) = last_error {
        return Err(SapperError::new(format!(
            "model returned no valid JSON object; nearest={e:?}"
        )));
    }
    Err(SapperError::new(format!(
        "no JSON object in model output: {:?}",
        text.chars().take(200).collect::<String>()
    )))
}

/// Cheap snapshot to detect whether a step changed the graph.
pub fn graph_signature(
    store: &Store,
) -> Result<(
    String,
    Vec<(String, String)>,
    Vec<(String, Option<String>, Option<String>)>,
)> {
    let g = build_graph_view(store)?;
    let facts = g
        .facts
        .iter()
        .map(|f| {
            (
                f["id"].as_str().unwrap_or("").to_string(),
                f.get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            )
        })
        .collect();
    let intents = g
        .intents
        .iter()
        .map(|i| {
            (
                i["id"].as_str().unwrap_or("").to_string(),
                i["to"].as_str().map(|s| s.to_string()),
                i.get("confirmed_at")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
            )
        })
        .collect();
    Ok((
        g.project["status"].as_str().unwrap_or("").to_string(),
        facts,
        intents,
    ))
}

/// Refuse to delete a path that doesn't look like a fact-graph store.
pub fn assert_safe_store_root(root: &Path, action: &str) -> Result<()> {
    if root == root.parent().unwrap_or_else(|| Path::new("/")) {
        return Err(SapperError::new(format!(
            "refusing to {action} filesystem root: {}",
            root.display()
        )));
    }
    if let Ok(home) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
        let home = Path::new(&home);
        if let (Ok(a), Ok(b)) = (root.canonicalize(), home.canonicalize()) {
            if a == b {
                return Err(SapperError::new(format!(
                    "refusing to {action} home directory: {}",
                    root.display()
                )));
            }
        }
    }
    if root.exists() && !root.join("project.json").exists() {
        return Err(SapperError::new(format!(
            "refusing to {action} {}: not a fact-graph store (no project.json). Re-check your --store path.",
            root.display()
        )));
    }
    Ok(())
}

/// Parse CLI `--from a,b c` into deduplicated ids.
pub fn parse_from(raw: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = vec![];
    for s in raw.split(|c: char| c.is_whitespace() || c == ',') {
        let s = s.trim();
        if !s.is_empty() && seen.insert(s.to_string()) {
            out.push(s.to_string());
        }
    }
    out
}

/// Parse CLI `--data key=value` repeatable args into a Map.
pub fn parse_kv_pairs(pairs: &[String]) -> Result<serde_json::Map<String, Value>> {
    let mut out = serde_json::Map::new();
    for item in pairs {
        let Some(eq) = item.find('=') else {
            return Err(SapperError::new(format!(
                "--data expects key=value, got {item:?}"
            )));
        };
        let key = item[..eq].trim().to_string();
        if key.is_empty() {
            return Err(SapperError::new(format!(
                "--data key cannot be empty: {item:?}"
            )));
        }
        out.insert(key, Value::String(item[eq + 1..].to_string()));
    }
    Ok(out)
}
