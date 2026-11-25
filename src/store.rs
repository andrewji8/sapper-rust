//! Filesystem-backed fact graph. One JSON file per node under `.sapper/`,
//! guarded by a cross-platform exclusive lock (flock on Unix,
//! LockFileEx on Windows via `fs2`).

use crate::error::{Result, SapperError};
pub use crate::util::read_json;
use crate::util::{dump_json, is_valid_id, now_iso, FACT_KINDS, GOAL_ID, LOCK_NAME};
use fs2::FileExt;
use serde_json::{Map, Value};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Filesystem-backed fact graph.
pub struct Store {
    pub root: PathBuf,
    pub facts_dir: PathBuf,
    pub intents_dir: PathBuf,
    pub hints_dir: PathBuf,
    pub project_file: PathBuf,
    pub lock_file: PathBuf,
}

/// RAII guard around the graph's exclusive lock (`.sapper/.lock`).
/// Held for the scope of a mutating operation.
pub struct Lock {
    file: File,
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let facts_dir = root.join("facts");
        let intents_dir = root.join("intents");
        let hints_dir = root.join("hints");
        let project_file = root.join("project.json");
        let lock_file = root.join(LOCK_NAME);
        Store {
            root,
            facts_dir,
            intents_dir,
            hints_dir,
            project_file,
            lock_file,
        }
    }

    pub fn exists(&self) -> bool {
        self.project_file.exists()
    }

    pub fn require(&self) -> Result<()> {
        if !self.exists() {
            return Err(SapperError::new(format!(
                "no fact graph here ({}). run `sapper init` first, or use --store <path>.",
                self.root.display()
            )));
        }
        Ok(())
    }

    /// Exclusive cross-platform lock on `.sapper/.lock` (flock / LockFileEx).
    pub fn locked(&self) -> Result<Lock> {
        if let Some(parent) = self.lock_file.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = open_options_ext::open_append_create(&self.lock_file)?;
        file.lock_exclusive().map_err(|e| {
            SapperError::new(format!("cannot lock {}: {e}", self.lock_file.display()))
        })?;
        Ok(Lock { file })
    }

    // -- init ---------------------------------------------------------------
    pub fn init(&self, origin: &str, goal: &str) -> Result<()> {
        let goal = goal.trim();
        if goal.is_empty() {
            return Err(SapperError::new("goal cannot be empty"));
        }
        let _lock = self.locked()?;
        if self.exists() {
            return Err(SapperError::new(format!(
                "fact graph already exists at {}",
                self.root.display()
            )));
        }
        // leftovers check (skip the lock file)
        let mut leftovers: Vec<String> = vec![];
        if let Ok(entries) = fs::read_dir(&self.root) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name != LOCK_NAME {
                    leftovers.push(name);
                }
            }
        }
        leftovers.sort();
        if !leftovers.is_empty() {
            let shown = leftovers
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            return Err(SapperError::new(format!(
                "cannot initialize graph in non-empty directory {}; found: {shown}",
                self.root.display()
            )));
        }
        for d in [
            &self.root,
            &self.facts_dir,
            &self.intents_dir,
            &self.hints_dir,
        ] {
            fs::create_dir_all(d)?;
        }
        let mut project = Map::new();
        project.insert("id".into(), Value::String("proj".into()));
        project.insert("origin".into(), Value::String(origin.trim().to_string()));
        project.insert("goal".into(), Value::String(goal.to_string()));
        project.insert("status".into(), Value::String("active".into()));
        project.insert("created_at".into(), Value::String(now_iso()));
        let mut counters = Map::new();
        for k in ["fact", "intent", "hint", "run"] {
            counters.insert(k.to_string(), Value::from(0));
        }
        project.insert("counters".into(), Value::Object(counters));
        atomic_write_json(&self.project_file, &Value::Object(project))?;

        let goal_fact = serde_json::json!({
            "id": GOAL_ID,
            "description": goal,
            "created_at": now_iso(),
            "creator": "system",
            "source_intent": Value::Null,
        });
        atomic_write_json(&self.facts_dir.join(format!("{GOAL_ID}.json")), &goal_fact)?;
        Ok(())
    }

    // -- project ------------------------------------------------------------
    pub fn load_project(&self) -> Result<Value> {
        let project = read_json(&self.project_file).map_err(|e| {
            SapperError::new(format!(
                "cannot read project metadata {}: {}",
                self.project_file.display(),
                e.0
            ))
        })?;
        if !project.is_object() {
            return Err(SapperError::new(
                "invalid project.json: expected a JSON object",
            ));
        }
        for key in ["id", "origin", "goal", "status"] {
            match project.get(key) {
                Some(Value::String(_)) => {}
                _ => {
                    return Err(SapperError::new(format!(
                        "invalid project.json: '{key}' must be a string"
                    )));
                }
            }
        }
        let goal = project["goal"].as_str().unwrap_or("").trim();
        if goal.is_empty() {
            return Err(SapperError::new(
                "invalid project.json: 'goal' cannot be empty",
            ));
        }
        let mut v = project.clone();
        if !v.get("counters").is_some_and(|c| c.is_object()) {
            v["counters"] = Value::Object(Map::new());
        }
        let counters = v.get_mut("counters").unwrap();
        let counters = counters.as_object_mut().unwrap();
        for kind in ["fact", "intent", "hint", "run"] {
            let c = counters.get(kind).cloned().unwrap_or(Value::from(0));
            let n = match &c {
                Value::Number(n) if !n.is_f64() && n.as_i64().unwrap_or(0) >= 0 => {
                    n.as_i64().unwrap_or(0)
                }
                _ => {
                    return Err(SapperError::new(format!(
                        "invalid project.json: counter {kind:?} must be a non-negative integer"
                    )));
                }
            };
            counters.insert(kind.to_string(), Value::from(n));
        }
        Ok(v)
    }

    pub fn save_project(&self, project: &Value) -> Result<()> {
        atomic_write_json(&self.project_file, project)
    }

    // -- id allocation (must be called while holding the lock) --------------
    pub fn next_id(&self, kind: &str, prefix: &str) -> Result<String> {
        let mut project = self.load_project()?;
        let counter = project
            .get("counters")
            .and_then(|c| c.get(kind))
            .and_then(|c| c.as_i64())
            .unwrap_or(0);
        let mut n = counter + 1;
        let dir = match kind {
            "fact" => &self.facts_dir,
            "intent" => &self.intents_dir,
            "hint" => &self.hints_dir,
            _ => &PathBuf::new(),
        };
        while dir.exists() && dir.join(format!("{prefix}{n:03}.json")).exists() {
            n += 1;
        }
        project
            .as_object_mut()
            .ok_or_else(|| SapperError::new("internal: project must be an object"))?
            .get_mut("counters")
            .ok_or_else(|| SapperError::new("internal: missing counters"))?
            .as_object_mut()
            .ok_or_else(|| SapperError::new("internal: counters must be an object"))?
            .insert(kind.to_string(), Value::from(n));
        self.save_project(&project)?;
        Ok(format!("{prefix}{n:03}"))
    }

    // -- facts ---------------------------------------------------------------
    pub fn list_facts(&self) -> Result<Vec<Value>> {
        read_node_files(&self.facts_dir, "fact")
    }

    pub fn get_fact(&self, fid: &str) -> Result<Value> {
        validate_lookup_id(fid, "fact")?;
        let p = self.facts_dir.join(format!("{fid}.json"));
        if !p.exists() {
            return Err(SapperError::new(format!("fact {fid} not found")));
        }
        read_node(&p, "fact")
    }

    pub fn add_fact(
        &self,
        description: &str,
        creator: &str,
        source_intent: Option<&str>,
        title: Option<&str>,
        doc: Option<&str>,
        kind: Option<&str>,
        data: Option<&Map<String, Value>>,
    ) -> Result<Value> {
        if description.trim().is_empty() {
            return Err(SapperError::new("fact description cannot be empty"));
        }
        if creator.trim().is_empty() {
            return Err(SapperError::new("fact creator cannot be empty"));
        }
        if let Some(si) = source_intent {
            validate_lookup_id(si, "intent")?;
        }
        let kind = normalize_fact_kind(kind)?;
        let data = normalize_fact_data(data)?;
        let _lock = self.locked()?;
        let mut source = None;
        if let Some(si) = source_intent {
            let s = self.get_intent(si)?;
            if !s["to"].is_null() {
                return Err(SapperError::new(format!("intent {si} already concluded")));
            }
            check_conclusion_permission(&s, Some(creator), false)?;
            source = Some(s);
        }
        let fid = self.next_id("fact", "f")?;
        let mut fact = Map::new();
        fact.insert("id".into(), Value::String(fid.clone()));
        fact.insert(
            "title".into(),
            match title {
                Some(t) if !t.trim().is_empty() => Value::String(t.trim().to_string()),
                _ => Value::Null,
            },
        );
        fact.insert(
            "description".into(),
            Value::String(description.trim().to_string()),
        );
        fact.insert(
            "doc".into(),
            match doc {
                Some(d) if !d.trim().is_empty() => Value::String(d.trim().to_string()),
                _ => Value::Null,
            },
        );
        fact.insert("kind".into(), kind.map_or(Value::Null, Value::String));
        fact.insert("data".into(), data);
        fact.insert("created_at".into(), Value::String(now_iso()));
        fact.insert("creator".into(), Value::String(creator.to_string()));
        fact.insert(
            "source_intent".into(),
            source_intent.map_or(Value::Null, |s| Value::String(s.to_string())),
        );
        let fact = Value::Object(fact);
        atomic_write_json(&self.facts_dir.join(format!("{fid}.json")), &fact)?;
        if let Some(mut s) = source {
            s["to"] = Value::String(fid.clone());
            s["status"] = Value::String("done".into());
            s["concluded_at"] = Value::String(now_iso());
            s["worker"] = Value::String(creator.to_string());
            self.save_intent(&s)?;
        }
        Ok(fact)
    }

    // -- intents --------------------------------------------------------------
    pub fn list_intents(&self) -> Result<Vec<Value>> {
        read_node_files(&self.intents_dir, "intent")
    }

    pub fn get_intent(&self, iid: &str) -> Result<Value> {
        validate_lookup_id(iid, "intent")?;
        let p = self.intents_dir.join(format!("{iid}.json"));
        if !p.exists() {
            return Err(SapperError::new(format!("intent {iid} not found")));
        }
        read_node(&p, "intent")
    }

    pub fn add_intent(
        &self,
        from_ids: Vec<String>,
        description: &str,
        creator: &str,
        requires_confirmation: bool,
        title: Option<&str>,
        doc: Option<&str>,
    ) -> Result<Value> {
        if description.trim().is_empty() {
            return Err(SapperError::new("intent description cannot be empty"));
        }
        if creator.trim().is_empty() {
            return Err(SapperError::new("intent creator cannot be empty"));
        }
        // de-duplicate while preserving order
        let mut seen = std::collections::HashSet::new();
        let from_ids: Vec<String> = from_ids
            .into_iter()
            .filter(|s| seen.insert(s.clone()))
            .collect();
        let _lock = self.locked()?;
        self.validate_from(&from_ids)?;
        let iid = self.next_id("intent", "i")?;
        let mut intent = Map::new();
        intent.insert("id".into(), Value::String(iid.clone()));
        intent.insert(
            "title".into(),
            match title {
                Some(t) if !t.trim().is_empty() => Value::String(t.trim().to_string()),
                _ => Value::Null,
            },
        );
        intent.insert(
            "from".into(),
            Value::Array(from_ids.iter().map(|s| Value::String(s.clone())).collect()),
        );
        intent.insert("to".into(), Value::Null);
        intent.insert(
            "description".into(),
            Value::String(description.trim().to_string()),
        );
        intent.insert(
            "doc".into(),
            match doc {
                Some(d) if !d.trim().is_empty() => Value::String(d.trim().to_string()),
                _ => Value::Null,
            },
        );
        intent.insert("creator".into(), Value::String(creator.to_string()));
        intent.insert("status".into(), Value::String("open".into()));
        intent.insert("worker".into(), Value::Null);
        intent.insert(
            "requires_confirmation".into(),
            Value::Bool(requires_confirmation),
        );
        intent.insert("confirmed_at".into(), Value::Null);
        intent.insert("created_at".into(), Value::String(now_iso()));
        intent.insert("concluded_at".into(), Value::Null);
        let intent = Value::Object(intent);
        atomic_write_json(&self.intents_dir.join(format!("{iid}.json")), &intent)?;
        Ok(intent)
    }

    pub fn save_intent(&self, intent: &Value) -> Result<()> {
        let id = intent["id"]
            .as_str()
            .ok_or_else(|| SapperError::new("intent record missing 'id'"))?;
        validate_lookup_id(id, "intent")?;
        atomic_write_json(&self.intents_dir.join(format!("{id}.json")), intent)
    }

    fn validate_from(&self, fact_ids: &[String]) -> Result<()> {
        if fact_ids.is_empty() {
            return Err(SapperError::new(
                "intent must have at least one `from` fact",
            ));
        }
        let existing: std::collections::HashSet<String> = self
            .list_facts()?
            .iter()
            .filter_map(|f| f["id"].as_str().map(|s| s.to_string()))
            .collect();
        for fid in fact_ids {
            if !existing.contains(fid.as_str()) {
                return Err(SapperError::new(format!("from fact {fid} does not exist")));
            }
            if fid == GOAL_ID {
                return Err(SapperError::new("`goal` cannot be used in `from`"));
            }
            validate_lookup_id(fid, "fact")?;
        }
        Ok(())
    }

    /// Update a fact/intent doc under the graph lock.
    /// `expected_doc: Some(v)` enforces the value the editor saw.
    pub fn update_doc(
        &self,
        node_id: &str,
        doc: Option<&str>,
        expected_doc: Option<&Option<Value>>,
    ) -> Result<Value> {
        validate_lookup_id(node_id, "node")?;
        let _lock = self.locked()?;
        let candidates: Vec<(String, PathBuf)> = if node_id == GOAL_ID {
            vec![(
                "fact".into(),
                self.facts_dir.join(format!("{node_id}.json")),
            )]
        } else {
            vec![
                (
                    "fact".into(),
                    self.facts_dir.join(format!("{node_id}.json")),
                ),
                (
                    "intent".into(),
                    self.intents_dir.join(format!("{node_id}.json")),
                ),
            ]
        };
        for (kind, path) in candidates {
            if !path.exists() {
                continue;
            }
            let mut node = read_node(&path, &kind)?;
            let node = node
                .as_object_mut()
                .ok_or_else(|| SapperError::new("internal: node must be an object"))?;
            if let Some(Some(expected)) = expected_doc {
                if &node["doc"] != expected {
                    return Err(SapperError::new(format!(
                        "{node_id} changed while the editor was open; re-read it and try again"
                    )));
                }
            }
            let cleaned: Option<Value> = doc.and_then(|d| {
                let t = d.trim();
                if t.is_empty() {
                    None
                } else {
                    Some(Value::String(t.to_string()))
                }
            });
            node.insert("doc".into(), cleaned.unwrap_or(Value::Null));
            let node = Value::Object(std::mem::take(node));
            atomic_write_json(&path, &node)?;
            return Ok(node);
        }
        Err(SapperError::new(format!(
            "no fact or intent named {node_id}"
        )))
    }

    #[allow(dead_code)]
    /// Link an intent to a produced fact (or "goal"). Confirmation and
    /// ownership are the final write-side guard.
    pub fn conclude_intent(
        &self,
        iid: &str,
        to_fact_id: &str,
        worker: Option<&str>,
        allow_unconfirmed: bool,
    ) -> Result<Value> {
        let _lock = self.locked()?;
        let mut intent = self.get_intent(iid)?;
        let intent = intent
            .as_object_mut()
            .ok_or_else(|| SapperError::new("internal: node must be an object"))?;
        if !intent["to"].is_null() {
            return Err(SapperError::new(format!(
                "intent {iid} already concluded (to={})",
                intent["to"].as_str().unwrap_or("?")
            )));
        }
        check_conclusion_permission(&Value::Object(intent.clone()), worker, allow_unconfirmed)?;
        if to_fact_id != GOAL_ID {
            validate_lookup_id(to_fact_id, "fact")?;
        } else {
            let project = self.load_project()?;
            let status = project["status"].as_str().unwrap_or("");
            if status == "completed" {
                return Err(SapperError::new("project is already completed"));
            }
            if status != "active" {
                return Err(SapperError::new(format!(
                    "project status {status:?} cannot be completed"
                )));
            }
            for other in self.list_intents()? {
                if other["id"] != iid && other["to"] == Value::String(GOAL_ID.to_string()) {
                    return Err(SapperError::new("project already has a completion edge"));
                }
            }
        }
        if to_fact_id != GOAL_ID {
            let mut target = self.get_fact(to_fact_id)?;
            let target = target
                .as_object_mut()
                .ok_or_else(|| SapperError::new("internal: node must be an object"))?;
            let source_intent = target.get("source_intent").cloned().unwrap_or(Value::Null);
            match source_intent {
                Value::Null => {}
                Value::String(s) if s == iid => {}
                other => {
                    return Err(SapperError::new(format!(
                        "fact {to_fact_id} already belongs to intent {}",
                        other.as_str().unwrap_or("?")
                    )))
                }
            }
            if target.get("source_intent").is_none_or(|v| v.is_null()) {
                target.insert("source_intent".into(), Value::String(iid.to_string()));
                atomic_write_json(
                    &self.facts_dir.join(format!("{to_fact_id}.json")),
                    &Value::Object(target.clone()),
                )?;
            }
        }
        intent.insert("to".into(), Value::String(to_fact_id.to_string()));
        intent.insert(
            "status".into(),
            Value::String(if to_fact_id == GOAL_ID {
                "complete".into()
            } else {
                "done".into()
            }),
        );
        intent.insert("concluded_at".into(), Value::String(now_iso()));
        if let Some(w) = worker {
            intent.insert("worker".into(), Value::String(w.to_string()));
        }
        let intent = Value::Object(std::mem::take(intent));
        self.save_intent(&intent)?;
        if to_fact_id == GOAL_ID {
            let mut project = self.load_project()?;
            project["status"] = Value::String("completed".into());
            project["completed_at"] = Value::String(now_iso());
            self.save_project(&project)?;
        }
        Ok(intent)
    }

    /// Create the terminal goal edge atomically with the project status.
    pub fn complete_project(
        &self,
        from_ids: Vec<String>,
        note: &str,
        worker: &str,
    ) -> Result<Value> {
        if note.trim().is_empty() {
            return Err(SapperError::new("completion note cannot be empty"));
        }
        if worker.trim().is_empty() {
            return Err(SapperError::new("completion worker cannot be empty"));
        }
        let _lock = self.locked()?;
        let project = self.load_project()?;
        let status = project["status"].as_str().unwrap_or("");
        if status == "completed" {
            return Err(SapperError::new("project is already completed"));
        }
        if status != "active" {
            return Err(SapperError::new(format!(
                "project status {status:?} cannot be completed"
            )));
        }
        let has_goal_edge = self
            .list_intents()?
            .iter()
            .any(|i| i["to"] == Value::String(GOAL_ID.to_string()));
        if has_goal_edge {
            return Err(SapperError::new("project already has a completion edge"));
        }
        self.validate_from(&from_ids)?;
        let mut seen = std::collections::HashSet::new();
        let from_ids: Vec<String> = from_ids
            .into_iter()
            .filter(|s| seen.insert(s.clone()))
            .collect();
        let iid = self.next_id("intent", "i")?;
        let now = now_iso();
        let mut intent = Map::new();
        intent.insert("id".into(), Value::String(iid.clone()));
        intent.insert("title".into(), Value::Null);
        intent.insert(
            "from".into(),
            Value::Array(from_ids.iter().map(|s| Value::String(s.clone())).collect()),
        );
        intent.insert("to".into(), Value::String(GOAL_ID.to_string()));
        intent.insert("description".into(), Value::String(note.trim().to_string()));
        intent.insert("doc".into(), Value::Null);
        intent.insert("creator".into(), Value::String(worker.to_string()));
        intent.insert("status".into(), Value::String("complete".into()));
        intent.insert("worker".into(), Value::String(worker.to_string()));
        intent.insert("requires_confirmation".into(), Value::Bool(false));
        intent.insert("confirmed_at".into(), Value::Null);
        intent.insert("created_at".into(), Value::String(now.clone()));
        intent.insert("concluded_at".into(), Value::String(now));
        let intent = Value::Object(intent);
        atomic_write_json(&self.intents_dir.join(format!("{iid}.json")), &intent)?;
        let mut project = self.load_project()?;
        project["status"] = Value::String("completed".into());
        project["completed_at"] = Value::String(now_iso());
        self.save_project(&project)?;
        Ok(intent)
    }

    /// Atomically: re-check the intent is still open under lock, allocate a
    /// new fact id, write the fact, and link the intent to it 鈥?one locked
    /// transaction. Prevents orphan facts and the duplicate-worker race.
    pub fn conclude_with_new_fact(
        &self,
        iid: &str,
        description: &str,
        worker: &str,
        title: Option<&str>,
        doc: Option<&str>,
        kind: Option<&str>,
        data: Option<&Map<String, Value>>,
        allow_unconfirmed: bool,
    ) -> Result<(Value, Value)> {
        let kind = normalize_fact_kind(kind)?;
        let data = normalize_fact_data(data)?;
        if description.trim().is_empty() {
            return Err(SapperError::new("fact description cannot be empty"));
        }
        let _lock = self.locked()?;
        let mut intent = self.get_intent(iid)?;
        let intent = intent
            .as_object_mut()
            .ok_or_else(|| SapperError::new("internal: node must be an object"))?;
        if !intent["to"].is_null() {
            return Err(SapperError::new(format!(
                "intent {iid} already concluded (to={}); dropping duplicate executor result",
                intent["to"].as_str().unwrap_or("?")
            )));
        }
        check_conclusion_permission(
            &Value::Object(intent.clone()),
            Some(worker),
            allow_unconfirmed,
        )?;
        let fid = self.next_id("fact", "f")?;
        let now = now_iso();
        let mut fact = Map::new();
        fact.insert("id".into(), Value::String(fid.clone()));
        fact.insert(
            "title".into(),
            match title {
                Some(t) if !t.trim().is_empty() => Value::String(t.trim().to_string()),
                _ => Value::Null,
            },
        );
        fact.insert(
            "description".into(),
            Value::String(description.trim().to_string()),
        );
        fact.insert(
            "doc".into(),
            match doc {
                Some(d) if !d.trim().is_empty() => Value::String(d.trim().to_string()),
                _ => Value::Null,
            },
        );
        fact.insert("kind".into(), kind.map_or(Value::Null, Value::String));
        fact.insert("data".into(), data);
        fact.insert("created_at".into(), Value::String(now.clone()));
        fact.insert("creator".into(), Value::String(worker.to_string()));
        fact.insert("source_intent".into(), Value::String(iid.to_string()));
        let fact = Value::Object(fact);
        atomic_write_json(&self.facts_dir.join(format!("{fid}.json")), &fact)?;
        intent.insert("to".into(), Value::String(fid.clone()));
        intent.insert("status".into(), Value::String("done".into()));
        intent.insert("concluded_at".into(), Value::String(now));
        intent.insert("worker".into(), Value::String(worker.to_string()));
        let intent = Value::Object(std::mem::take(intent));
        self.save_intent(&intent)?;
        Ok((fact, intent))
    }

    // -- claim / release / confirm -------------------------------------------
    pub fn claim_intent(&self, iid: &str, worker: &str) -> Result<Value> {
        let _lock = self.locked()?;
        let mut intent = self.get_intent(iid)?;
        let intent = intent
            .as_object_mut()
            .ok_or_else(|| SapperError::new("internal: node must be an object"))?;
        if !intent["to"].is_null() {
            return Err(SapperError::new(format!("intent {iid} already concluded")));
        }
        let needs_confirm = intent
            .get("requires_confirmation")
            .and_then(|v| v.as_bool());
        let confirmed = intent
            .get("confirmed_at")
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty());
        if needs_confirm == Some(true) && confirmed != Some(true) {
            return Err(SapperError::new(format!(
                "intent {iid} requires confirmation before it can be claimed"
            )));
        }
        let owner = intent.get("worker").and_then(|v| v.as_str());
        if let Some(owner) = owner {
            if !owner.is_empty() && owner != worker {
                return Err(SapperError::new(format!("intent {iid} claimed by {owner}")));
            }
        }
        intent.insert("worker".into(), Value::String(worker.to_string()));
        intent.insert("status".into(), Value::String("claimed".into()));
        let intent = Value::Object(std::mem::take(intent));
        self.save_intent(&intent)?;
        Ok(intent)
    }

    pub fn release_intent(&self, iid: &str, worker: &str) -> Result<Value> {
        let _lock = self.locked()?;
        let mut intent = self.get_intent(iid)?;
        let intent = intent
            .as_object_mut()
            .ok_or_else(|| SapperError::new("internal: node must be an object"))?;
        if !intent["to"].is_null() {
            return Err(SapperError::new(format!("intent {iid} already concluded")));
        }
        let owner = intent.get("worker").and_then(|v| v.as_str());
        if let Some(owner) = owner {
            if !owner.is_empty() && owner != worker {
                return Err(SapperError::new(format!("intent {iid} claimed by {owner}")));
            }
        }
        intent.insert("worker".into(), Value::Null);
        intent.insert("status".into(), Value::String("open".into()));
        let intent = Value::Object(std::mem::take(intent));
        self.save_intent(&intent)?;
        Ok(intent)
    }

    pub fn confirm_intent(&self, iid: &str) -> Result<Value> {
        let _lock = self.locked()?;
        let mut intent = self.get_intent(iid)?;
        let intent = intent
            .as_object_mut()
            .ok_or_else(|| SapperError::new("internal: node must be an object"))?;
        if !intent["to"].is_null() {
            return Err(SapperError::new(format!("intent {iid} already concluded")));
        }
        if intent
            .get("requires_confirmation")
            .and_then(|v| v.as_bool())
            != Some(true)
        {
            return Err(SapperError::new(format!(
                "intent {iid} does not require confirmation"
            )));
        }
        if intent
            .get("confirmed_at")
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false)
        {
            return Err(SapperError::new(format!("intent {iid} already confirmed")));
        }
        intent.insert("confirmed_at".into(), Value::String(now_iso()));
        let intent = Value::Object(std::mem::take(intent));
        self.save_intent(&intent)?;
        Ok(intent)
    }

    // -- hints ----------------------------------------------------------------
    pub fn list_hints(&self) -> Result<Vec<Value>> {
        read_node_files(&self.hints_dir, "hint")
    }

    pub fn add_hint(&self, content: &str, creator: &str) -> Result<Value> {
        if content.trim().is_empty() {
            return Err(SapperError::new("hint content cannot be empty"));
        }
        if creator.trim().is_empty() {
            return Err(SapperError::new("hint creator cannot be empty"));
        }
        let _lock = self.locked()?;
        let hid = self.next_id("hint", "h")?;
        let hint = serde_json::json!({
            "id": hid,
            "content": content.trim(),
            "creator": creator,
            "created_at": now_iso(),
        });
        atomic_write_json(&self.hints_dir.join(format!("{hid}.json")), &hint)?;
        Ok(hint)
    }

    /// Lifecycle update for a flag fact: recorded -> submitted/rejected/...
    pub fn update_flag_fact(
        &self,
        fid: &str,
        status: &str,
        result_note: Option<&str>,
    ) -> Result<Value> {
        const VALID: [&str; 4] = ["recorded", "submitted", "rejected", "duplicate"];
        if !VALID.contains(&status) {
            return Err(SapperError::new(format!("invalid flag status {status:?}")));
        }
        let _lock = self.locked()?;
        let mut fact = self.get_fact(fid)?;
        let fact = fact
            .as_object_mut()
            .ok_or_else(|| SapperError::new("internal: node must be an object"))?;
        if fact.get("kind").and_then(|v| v.as_str()) != Some("flag") {
            return Err(SapperError::new(format!("fact {fid} is not a flag fact")));
        }
        let mut data = fact
            .get("data")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        data.insert("status".into(), Value::String(status.to_string()));
        if let Some(note) = result_note {
            data.insert("result".into(), Value::String(note.to_string()));
        }
        let data = normalize_fact_data(Some(&data))?;
        fact.insert("data".into(), data);
        let fact = Value::Object(std::mem::take(fact));
        atomic_write_json(&self.facts_dir.join(format!("{fid}.json")), &fact)?;
        Ok(fact)
    }
}

// --------------------------------------------------------------------------- #
// low-level io
// --------------------------------------------------------------------------- #

pub fn validate_lookup_id(node_id: &str, kind: &str) -> Result<()> {
    // `goal` is reserved and valid only as a fact / node lookup.
    if node_id == GOAL_ID && (kind == "fact" || kind == "node") {
        return Ok(());
    }
    if !is_valid_id(node_id) {
        return Err(SapperError::new(format!("invalid {kind} id {node_id:?}")));
    }
    Ok(())
}

pub fn read_node(path: &Path, kind: &str) -> Result<Value> {
    let data = read_json(path).map_err(|e| {
        SapperError::new(format!(
            "cannot read {kind} {}: {}",
            path.file_name().unwrap_or_default().to_string_lossy(),
            e.0
        ))
    })?;
    if !data.is_object() {
        return Err(SapperError::new(format!(
            "invalid {kind} {}: expected a JSON object",
            path.file_name().unwrap_or_default().to_string_lossy()
        )));
    }
    let errors = node_schema_errors(&data, kind);
    if !errors.is_empty() {
        return Err(SapperError::new(format!(
            "invalid {kind} {}: {}",
            path.file_name().unwrap_or_default().to_string_lossy(),
            errors.join("; ")
        )));
    }
    let id = data["id"].as_str().unwrap_or("");
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    if stem != id {
        return Err(SapperError::new(format!(
            "invalid {kind} {}: filename does not match id {id:?}",
            path.file_name().unwrap_or_default().to_string_lossy()
        )));
    }
    Ok(data)
}

/// Return schema errors for records used by renderers and graph logic.
pub fn node_schema_errors(data: &Value, kind: &str) -> Vec<String> {
    let Some(obj) = data.as_object() else {
        return vec!["expected a JSON object".to_string()];
    };
    let mut errors: Vec<String> = vec![];
    let node_id = obj.get("id");
    match node_id {
        None | Some(Value::Null) => errors.push("missing string 'id'".into()),
        Some(Value::String(id)) => {
            if id == GOAL_ID && kind != "fact" {
                errors.push("reserved 'id' is only valid for the goal fact".into());
            } else if id != GOAL_ID && !is_valid_id(id) {
                errors.push("invalid 'id'".into());
            }
        }
        Some(_) => errors.push("missing string 'id'".into()),
    }

    let has = |k: &str| obj.contains_key(k);
    let str_of = |k: &str| obj.get(k).and_then(|v| v.as_str());

    if kind == "fact" {
        if str_of("description").is_none() {
            errors.push("missing string 'description'".into());
        }
        if str_of("created_at").is_none() {
            errors.push("missing string 'created_at'".into());
        }
        if str_of("creator").is_none() {
            errors.push("missing string 'creator'".into());
        }
        if has("source_intent") {
            match obj["source_intent"] {
                Value::Null => {}
                Value::String(_) => {}
                _ => errors.push("'source_intent' must be a string or null".into()),
            }
        } else {
            errors.push("missing 'source_intent'".into());
        }
        if let Some(Value::String(_)) = obj.get("title") {
        } else if let Some(v) = obj.get("title") {
            if !v.is_null() {
                errors.push("'title' must be a string or null".into());
            }
        }
        if let Some(Value::String(_)) = obj.get("doc") {
        } else if let Some(v) = obj.get("doc") {
            if !v.is_null() {
                errors.push("'doc' must be a string or null".into());
            }
        }
        if let Some(k) = obj.get("kind") {
            match k {
                Value::Null => {}
                Value::String(s) if FACT_KINDS.contains(&s.as_str()) => {}
                _ => errors.push(format!(
                    "'kind' must be null or one of: {}",
                    FACT_KINDS.join(", ")
                )),
            }
        }
        if let Some(d) = obj.get("data") {
            match d {
                Value::Null => {}
                Value::Object(m) => {
                    for (k, v) in m {
                        if !k
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
                        {
                            errors.push("'data' key must match [A-Za-z0-9_.-]+".into());
                            break;
                        }
                        match v {
                            Value::String(_) | Value::Number(_) => {}
                            _ => {
                                errors.push("'data' values must be scalars".into());
                                break;
                            }
                        }
                    }
                }
                _ => errors.push("'data' must be an object or null".into()),
            }
        }
    } else if kind == "intent" {
        let from_ok = obj
            .get("from")
            .map(|v| {
                let arr = match v.as_array() {
                    Some(a) if !a.is_empty() => a,
                    _ => return false,
                };
                arr.iter().all(|x| {
                    let s = match x.as_str() {
                        Some(s) => s,
                        None => return false,
                    };
                    s != GOAL_ID && is_valid_id(s)
                })
            })
            .unwrap_or(false);
        if !from_ok {
            errors.push("'from' must be a list of strings".into());
        }
        let to = obj.get("to");
        if !has("to") {
            errors.push("'to' must be a string or null".into());
        } else {
            match to {
                Some(Value::Null) => {}
                Some(Value::String(s)) if *s == GOAL_ID || is_valid_id(s) => {}
                _ => errors.push("'to' must be a string or null".into()),
            }
        }
        if str_of("description").is_none() {
            errors.push("missing string 'description'".into());
        }
        if str_of("created_at").is_none() {
            errors.push("missing string 'created_at'".into());
        }
        let status_ok = str_of("status")
            .map(|s| matches!(s, "open" | "claimed" | "done" | "complete"))
            .unwrap_or(false);
        if !has("status") || str_of("status").is_none() {
            errors.push("missing string 'status'".into());
        } else if !status_ok {
            errors.push("invalid 'status'".into());
        }
        if !has("worker")
            || !matches!(
                obj.get("worker"),
                Some(Value::Null) | Some(Value::String(_))
            )
        {
            errors.push("'worker' must be a string or null".into());
        }
        if !matches!(obj.get("requires_confirmation"), Some(Value::Bool(_))) {
            errors.push("missing boolean 'requires_confirmation'".into());
        }
        if !has("confirmed_at")
            || !matches!(
                obj.get("confirmed_at"),
                Some(Value::Null) | Some(Value::String(_))
            )
        {
            errors.push("'confirmed_at' must be a string or null".into());
        }
        let status = str_of("status");
        let target = obj.get("to");
        if matches!(status, Some("done") | Some("complete")) && target == Some(&Value::Null) {
            errors.push("completed status requires a target".into());
        }
        if matches!(status, Some("open") | Some("claimed"))
            && target.map(|t| !t.is_null()).unwrap_or(false)
        {
            errors.push("open/claimed status cannot have a target".into());
        }
        if status == Some("complete") && target != Some(&Value::String(GOAL_ID.to_string())) {
            errors.push("complete status must target goal".into());
        }
        if status == Some("done") && target == Some(&Value::String(GOAL_ID.to_string())) {
            errors.push("done status cannot target goal".into());
        }
        if let Some(v) = obj.get("title") {
            if !v.is_null() && !v.is_string() {
                errors.push("'title' must be a string or null".into());
            }
        }
        if let Some(v) = obj.get("doc") {
            if !v.is_null() && !v.is_string() {
                errors.push("'doc' must be a string or null".into());
            }
        }
    } else if kind == "hint" {
        if str_of("content").is_none() {
            errors.push("missing string 'content'".into());
        }
        if str_of("creator").is_none() {
            errors.push("missing string 'creator'".into());
        }
        if str_of("created_at").is_none() {
            errors.push("missing string 'created_at'".into());
        }
    }
    errors
}

pub fn check_conclusion_permission(
    intent: &Value,
    worker: Option<&str>,
    allow_unconfirmed: bool,
) -> Result<()> {
    let needs = intent
        .get("requires_confirmation")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let confirmed = intent
        .get("confirmed_at")
        .and_then(|v| v.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    if needs && !confirmed && !allow_unconfirmed {
        let id = intent.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        return Err(SapperError::new(format!(
            "intent {id} requires confirmation; run `sapper confirm {id}` first"
        )));
    }
    if let Some(owner) = intent.get("worker").and_then(|v| v.as_str()) {
        if !owner.is_empty() && Some(owner) != worker {
            let id = intent.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            return Err(SapperError::new(format!("intent {id} claimed by {owner}")));
        }
    }
    Ok(())
}

/// List all node JSON files in `directory`, skipping malformed or
/// schema-broken files with a stderr warning (a corrupt node must NOT
/// crash status/graph/view 鈥?degrade and keep going).
pub fn read_node_files(directory: &Path, kind: &str) -> Result<Vec<Value>> {
    let mut out: Vec<Value> = vec![];
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let entries = if directory.is_dir() {
        fs::read_dir(directory)
            .map(|it| {
                it.flatten()
                    .map(|e| e.path())
                    .collect::<Vec<_>>()
                    .into_iter()
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
    let mut paths: Vec<PathBuf> = entries;
    paths.sort();
    for p in paths {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let stem = p
            .file_stem()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let data = match read_json(&p) {
            Ok(d) => d,
            Err(e) => {
                eprintln!(
                    "sapper: warning: skipping malformed {kind} file {name}: {}",
                    e.0
                );
                continue;
            }
        };
        let errors = node_schema_errors(&data, kind);
        if !errors.is_empty() {
            eprintln!(
                "sapper: warning: skipping invalid {kind} file {name}: {}",
                errors.join("; ")
            );
            continue;
        }
        let nid = match data["id"].as_str() {
            Some(s) => s.to_string(),
            None => {
                eprintln!("sapper: warning: skipping {kind} file {name}: missing id");
                continue;
            }
        };
        if stem != nid {
            eprintln!(
                "sapper: warning: skipping {kind} file {name}: filename does not match id {nid:?}"
            );
            continue;
        }
        if !seen.insert(nid.clone()) {
            eprintln!("sapper: warning: skipping duplicate {kind} id {nid:?} from {name}");
            continue;
        }
        out.push(data);
    }
    Ok(out)
}

/// Atomic JSON write: temp file in the destination directory, fsync, rename.
pub fn atomic_write_json(path: &Path, data: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(
        ".{name}.{}{}.tmp",
        std::process::id(),
        rand::random::<u32>()
    ));
    let result = (|| -> io::Result<()> {
        let mut f = File::create(&tmp)?;
        f.write_all(dump_json(data).as_bytes())?;
        f.sync_all()?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(SapperError::new(format!(
            "cannot write {}: {e}",
            path.display()
        )));
    }
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        SapperError::new(format!("cannot replace {}: {e}", path.display()))
    })?;
    Ok(())
}

// -- fact kind / data normalization ------------------------------------------

pub fn normalize_fact_kind(kind: Option<&str>) -> Result<Option<String>> {
    let Some(k) = kind else {
        return Ok(None);
    };
    let k = k.trim().to_lowercase();
    if k.is_empty() {
        return Ok(None);
    }
    if FACT_KINDS.contains(&k.as_str()) {
        Ok(Some(k))
    } else {
        Err(SapperError::new(format!(
            "unknown fact kind {kind:?}; expected one of: {}",
            FACT_KINDS.join(", ")
        )))
    }
}

/// Validate fact `data`: flat object, scalar values, safe keys.
pub fn normalize_fact_data(data: Option<&serde_json::Map<String, Value>>) -> Result<Value> {
    let Some(data) = data else {
        return Ok(Value::Null);
    };
    let mut out = Map::new();
    for (key, value) in data {
        if !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
        {
            return Err(SapperError::new(format!(
                "fact data key {key:?} must match [A-Za-z0-9_.-]+"
            )));
        }
        match value {
            Value::Null => {}
            Value::Bool(b) => {
                out.insert(key.clone(), Value::String(b.to_string()));
            }
            Value::String(s) => {
                out.insert(key.clone(), Value::String(s.clone()));
            }
            Value::Number(n) => {
                out.insert(key.clone(), Value::String(py_number_str(n)));
            }
            _ => {
                return Err(SapperError::new(format!(
                    "fact data value for {key:?} must be a scalar"
                )));
            }
        }
    }
    if out.is_empty() {
        Ok(Value::Null)
    } else {
        Ok(Value::Object(out))
    }
}

/// Python `str(value)` for scalars: ints stay ints, floats get the
/// shortest representation ("1.5" / "2.0").
pub fn py_number_str(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        i.to_string()
    } else if let Some(f) = n.as_f64() {
        if f.fract() == 0.0 && f.abs() < 1e15 {
            format!("{f:.1}")
        } else {
            let s = format!("{f}");
            // rust prints "1.5" fine; match python for common cases
            s
        }
    } else {
        n.to_string()
    }
}

// -- fs2 open helper -----------------------------------------------------------
mod open_options_ext {
    use std::fs::OpenOptions;
    use std::io::Result;
    use std::path::Path;

    /// Open (creating if needed) the lock file.
    ///
    /// Windows: fs2 implements flock with `LockFileEx`, which needs write
    /// access 鈥?opening append-only fails with ACCESS_DENIED (os error 5).
    /// Unix: flock works on any open mode; append+create mirrors the Python
    /// `os.open(path, O_APPEND | O_CREAT)` behavior.
    pub fn open_append_create(path: &Path) -> Result<std::fs::File> {
        #[cfg(windows)]
        {
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .open(path)
        }
        #[cfg(not(windows))]
        {
            OpenOptions::new().append(true).create(true).open(path)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_id_goal() {
        assert!(validate_lookup_id("goal", "fact").is_ok());
        assert!(validate_lookup_id("goal", "node").is_ok());
    }

    #[test]
    fn lookup_id_invalid() {
        assert!(validate_lookup_id("F001", "fact").is_err());
        assert!(validate_lookup_id("", "fact").is_err());
    }

    #[test]
    fn schema_fact_ok() {
        let v = serde_json::json!({
            "id": "f001",
            "description": "d",
            "created_at": "2026-01-01T00:00:00Z",
            "creator": "u",
            "source_intent": null,
            "title": null,
            "doc": null,
            "kind": null,
            "data": null
        });
        assert!(node_schema_errors(&v, "fact").is_empty());
    }

    #[test]
    fn schema_fact_missing() {
        let v = serde_json::json!({"id":"f001"});
        assert!(!node_schema_errors(&v, "fact").is_empty());
    }

    #[test]
    fn normalize_kind() {
        assert_eq!(
            normalize_fact_kind(Some("Host")).unwrap(),
            Some("host".to_string())
        );
        assert!(normalize_fact_kind(Some("nope")).is_err());
        assert_eq!(normalize_fact_kind(None).unwrap(), None);
    }
}
