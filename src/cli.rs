//! The sapper CLI — fact-graph working memory for agentic work.
//! Rust port of the Python `sapper` CLI with a Windows-compatible surface.

use crate::arena::ArenaClient;
use crate::auto::{cmd_auto, AutoOpts};
use crate::bench::{cmd_bench, BenchOpts};
use crate::command::{cmd_command, CommandOpts};
use crate::dispatch::{
    apply_blindspot, apply_brainstorm, brainstorm_pick, dispatch_reason_printed, load_template,
    parse_explore_payload, parse_verify_payload,
};
use crate::error::{Result, SapperError};
use crate::fleet::{cmd_fleet, FleetArgs, FLEET_FILE};
use crate::graph::assert_safe_store_root;
use crate::graph::{
    build_graph_view, extract_json_object, frontier, graph_signature, is_open, is_ready,
    parse_from, parse_kv_pairs, recall, render_graph_text, render_recall, render_status,
    validate_graph,
};
use crate::kb::{import_markdown, KnowledgeBase};
use crate::peers;
use crate::settings;
use crate::store::{normalize_fact_data, Store};
use crate::sweep::{cmd_sweep, SweepOpts};
use crate::util::{
    deadline_from_args, dump_json, edit_via_editor, expand_user, now_local_stamp, DEFAULT_STORE,
};
use crate::view;
use clap::{ArgAction, Parser};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Parser, Debug, Clone)]
#[command(
    name = "sapper",
    version,
    about = "fact-graph working memory for agentic work"
)]
struct Cli {
    #[arg(long, default_value = DEFAULT_STORE, global = true)]
    store: String,

    #[command(subcommand)]
    command: SubCommand,
}

#[derive(clap::Subcommand, Debug, Clone)]
enum SubCommand {
    /// create a fact graph in --store
    Init {
        #[arg(long, default_value = "")]
        origin: String,
        #[arg(long)]
        goal: String,
        #[arg(long)]
        force: bool,
    },
    /// project + frontier
    Status,
    /// dump the graph
    Graph {
        #[arg(long, value_enum, default_value = "text")]
        format: Format,
    },
    /// audit graph JSON, references, and project status without changing files
    Validate,
    /// open intents ready to work
    Frontier,
    /// pick the next intent to work (prints id)
    Pick {
        #[arg(long)]
        claim: bool,
        #[arg(long)]
        any: bool,
        #[arg(long)]
        worker: Option<String>,
    },
    /// add a confirmed fact (optionally typed: host/service/cred/vuln/loot/flag)
    Fact {
        description: String,
        #[arg(short, long)]
        title: Option<String>,
        #[arg(short = 'k', long)]
        kind: Option<String>,
        #[arg(short, long, action = ArgAction::Append, num_args = 1)]
        data: Vec<String>,
        #[arg(long)]
        doc: Option<String>,
        #[arg(long)]
        doc_file: Option<String>,
        #[arg(long, default_value = "user")]
        creator: String,
        #[arg(long)]
        from_intent: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// add an intent (a unit of work)
    Intent {
        #[arg(long = "from", required = true, visible_alias = "from")]
        from: String,
        description: String,
        #[arg(short, long)]
        title: Option<String>,
        #[arg(long)]
        doc: Option<String>,
        #[arg(long)]
        doc_file: Option<String>,
        #[arg(long, default_value = "user")]
        creator: String,
        #[arg(long)]
        confirm: bool,
        #[arg(long)]
        json: bool,
    },
    /// add or list human hints
    Hint {
        #[arg(required = false)]
        content: Option<String>,
        #[arg(long, default_value = "user")]
        creator: String,
        #[arg(long)]
        list: bool,
    },
    /// discover tmux panes and manage user-authorized AI peers
    Peers {
        #[arg(long)]
        discover: bool,
        #[arg(long)]
        add: Option<String>,
        #[arg(long)]
        remove: Option<String>,
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        sender: Option<String>,
        #[arg(long)]
        channel: Option<String>,
        #[arg(long)]
        note: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// send a message to a user-authorized tmux peer
    Send {
        peer: String,
        #[arg(required = false)]
        message: Vec<String>,
        #[arg(long)]
        sender: Option<String>,
        #[arg(long)]
        channel: Option<String>,
        #[arg(long)]
        no_log: bool,
        #[arg(long)]
        raw: bool,
    },
    /// read recent output from a user-authorized tmux peer
    Capture {
        peer: String,
        #[arg(long, default_value_t = 120)]
        lines: i64,
        #[arg(long)]
        channel: Option<String>,
        #[arg(long)]
        log: bool,
    },
    /// show project | fact <id> | intent <id>
    Show {
        #[arg(value_parser = ["project", "fact", "intent"])]
        entity: String,
        #[arg(required = false)]
        id: Option<String>,
    },
    /// read or write a node's detailed document (the third layer)
    Doc {
        id: String,
        #[arg(long)]
        set: Option<String>,
        #[arg(long)]
        set_file: Option<String>,
        #[arg(long)]
        edit: bool,
    },
    /// find prior facts/intents similar to <query>
    Recall {
        query: String,
        #[arg(short = 'n', long, default_value_t = 5)]
        limit: i64,
        #[arg(long, default_value = "any", value_parser = ["fact", "intent", "any"])]
        kind: String,
        #[arg(long)]
        json: bool,
    },
    /// claim an open intent
    Claim {
        id: String,
        #[arg(long, required = true)]
        worker: String,
    },
    /// release a claimed intent
    Release {
        id: String,
        #[arg(long)]
        worker: Option<String>,
    },
    /// conclude an intent with a produced fact
    Done {
        id: String,
        #[arg(long, required = true)]
        fact: String,
        #[arg(short, long)]
        title: Option<String>,
        #[arg(short = 'k', long)]
        kind: Option<String>,
        #[arg(short, long, action = ArgAction::Append)]
        data: Vec<String>,
        #[arg(long)]
        doc: Option<String>,
        #[arg(long)]
        doc_file: Option<String>,
        #[arg(long, default_value = "agent")]
        worker: String,
    },
    /// mark the project goal satisfied
    Complete {
        #[arg(long = "from", required = true)]
        from: String,
        #[arg(long)]
        note: Option<String>,
        #[arg(long, default_value = "reason")]
        worker: String,
    },
    /// confirm an intent that required it
    Confirm { id: String },
    /// run a commander/executor step via the engine CLI
    Dispatch {
        target: String,
        #[arg(long, default_value = "sonnet")]
        model: String,
        #[arg(long, default_value_t = 600)]
        timeout: u64,
        #[arg(long)]
        templates: Option<String>,
        #[arg(long)]
        skip_permissions: bool,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        claim: bool,
        #[arg(long)]
        intent: Option<String>,
        #[arg(long, default_value_t = 4)]
        max_options: i64,
    },
    /// pick one brainstorm option, abandon the rest
    BrainstormPick { id: String },
    /// opt-in THIS project: create .sapper/ + register hooks in ./.claude/settings.json
    Setup {
        #[arg(long)]
        goal: Option<String>,
        #[arg(long, default_value = "")]
        origin: String,
        #[arg(long)]
        agents: bool,
        #[arg(long)]
        force: bool,
    },
    /// remove hooks + AGENTS.md from THIS project (keeps .sapper/ unless --purge)
    Teardown {
        #[arg(long)]
        purge: bool,
    },
    /// open an interactive HTML visualization of the graph
    View {
        #[arg(long)]
        out: Option<String>,
        #[arg(long)]
        serve: bool,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 8765)]
        port: u16,
        #[arg(long)]
        print: bool,
    },
    /// record a captured flag (dedupe; optionally submit via arena)
    Flag {
        value: String,
        #[arg(short, long)]
        challenge: Option<String>,
        #[arg(long)]
        submit: bool,
        #[arg(short, long)]
        title: Option<String>,
        #[arg(long, default_value = "user")]
        creator: String,
    },
    /// competition platform bridge (TSecBench-style API)
    Arena {
        #[arg(value_parser = ["init", "challenges", "start", "stop", "submit", "hint"])]
        action: String,
        #[arg(required = false)]
        challenge_id: Option<String>,
        #[arg(long)]
        flag: Option<String>,
    },
    /// shared offline knowledge base (techniques / CVEs / writeups)
    Kb {
        #[arg(value_parser = ["add", "search", "show", "list", "import", "root"])]
        action: String,
        #[arg(required = false)]
        target: Option<String>,
        #[arg(short = 'd', long)]
        description: Option<String>,
        #[arg(long)]
        detail: Option<String>,
        #[arg(long)]
        detail_file: Option<String>,
        #[arg(short = 't', long, action = ArgAction::Append)]
        tag: Vec<String>,
        #[arg(short = 'c', long, action = ArgAction::Append)]
        command: Vec<String>,
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        path: Option<String>,
        #[arg(long)]
        root: Option<String>,
        #[arg(short = 'n', long, default_value_t = 5)]
        limit: i64,
    },
    /// drive TSecBench end-to-end: list->start->solve->submit->close
    Bench {
        #[arg(long, default_value = "runs")]
        root: String,
        #[arg(long)]
        minutes: Option<i64>,
        #[arg(long)]
        deadline: Option<String>,
        #[arg(long, default_value_t = 25)]
        minutes_per_challenge: i64,
        #[arg(long, default_value_t = 10)]
        max_steps: i64,
        #[arg(long, default_value_t = 20)]
        round_gap_seconds: i64,
        #[arg(long, default_value_t = 1)]
        parallel: i64,
        #[arg(long, default_value_t = 5)]
        poll_seconds: i64,
        #[arg(long, default_value = "sonnet")]
        model: String,
        #[arg(long)]
        templates: Option<String>,
        #[arg(long)]
        skip_permissions: bool,
        #[arg(long)]
        verify: bool,
    },
    /// field commander: LLM allocates the next work slice across challenge graphs
    Command {
        #[arg(long, default_value = "runs")]
        root: String,
        #[arg(long)]
        minutes: Option<i64>,
        #[arg(long)]
        deadline: Option<String>,
        #[arg(long, default_value_t = 20)]
        minutes_default: i64,
        #[arg(long, default_value_t = 40)]
        minutes_max: i64,
        #[arg(long)]
        fleet_file: Option<String>,
        #[arg(long, default_value_t = 30)]
        poll_seconds: i64,
        #[arg(long, default_value = "sonnet")]
        model: String,
        #[arg(long, default_value_t = 600)]
        timeout: u64,
        #[arg(long)]
        templates: Option<String>,
        #[arg(long)]
        skip_permissions: bool,
        #[arg(long)]
        verify: bool,
        #[arg(long, default_value_t = 8)]
        max_steps: i64,
    },
    /// launch/stop the whole formation in tmux panes (match-day)
    Fleet {
        #[arg(value_parser = ["init", "up", "down", "status", "hire", "fire", "workers"])]
        action: String,
        #[arg(long, default_value = FLEET_FILE)]
        file: String,
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        minutes: Option<i64>,
        #[arg(long)]
        max_steps: Option<i64>,
        #[arg(long)]
        extra_args: Option<String>,
    },
    /// round-robin `auto` across per-challenge graphs (<root>/<name>/.sapper)
    Sweep {
        #[arg(long, default_value = "runs")]
        root: String,
        #[arg(long)]
        minutes: Option<i64>,
        #[arg(long)]
        deadline: Option<String>,
        #[arg(long, default_value_t = 20)]
        minutes_per: i64,
        #[arg(long, default_value = "sonnet")]
        model: String,
        #[arg(long)]
        templates: Option<String>,
        #[arg(long)]
        skip_permissions: bool,
        #[arg(long)]
        verify: bool,
        #[arg(long, default_value_t = 8)]
        max_steps: i64,
    },
    /// drive the whole graph forward: reason -> explore -> verify -> repeat
    Auto {
        #[arg(long, default_value = "sonnet")]
        model: String,
        #[arg(long, default_value_t = 600)]
        timeout: u64,
        #[arg(long)]
        templates: Option<String>,
        #[arg(long)]
        skip_permissions: bool,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        verify: bool,
        #[arg(long, default_value_t = 8)]
        max_steps: i64,
        #[arg(long, default_value_t = 6)]
        max_explore: i64,
        #[arg(long)]
        minutes: Option<i64>,
        #[arg(long)]
        deadline: Option<String>,
    },
    /// Claude Code hook bridge (sessionstart | userpromptsubmit)
    Hook { event: String },
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq)]
enum Format {
    Text,
    Json,
}

pub fn run() -> i32 {
    let cli: Cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            return 1;
        }
    };
    let store_path = PathBuf::from(&cli.store);

    // templates default post-hoc (depends on store) for dispatch/auto/bench/command
    let effective_templates = |explicit: Option<&str>| -> PathBuf {
        if let Some(t) = explicit {
            PathBuf::from(t)
        } else {
            view::resolve_templates_dir(None, &store_path)
        }
    };

    let result: Result<i32> = run_command(&cli.command, &store_path, &effective_templates, &cli);
    match result {
        Ok(code) => code,
        Err(exc) => {
            eprintln!("sapper: {}", exc.0);
            1
        }
    }
}

fn run_command(
    cmd: &SubCommand,
    store_path: &Path,
    effective_templates: &impl Fn(Option<&str>) -> PathBuf,
    cli: &Cli,
) -> Result<i32> {
    match cmd {
        SubCommand::Init {
            origin,
            goal,
            force,
        } => cmd_init(store_path, origin, goal, *force),
        SubCommand::Status => {
            let store = Store::new(store_path);
            store.require()?;
            println!("{}", render_status(&store)?);
            Ok(0)
        }
        SubCommand::Graph { format } => {
            let store = Store::new(store_path);
            store.require()?;
            match format {
                Format::Json => {
                    let g = build_graph_view(&store)?;
                    println!("{}", dump_json(&crate::graph::graph_view_json(&g)));
                }
                Format::Text => println!("{}", render_graph_text(&store)?),
            }
            Ok(0)
        }
        SubCommand::Validate => {
            let store = Store::new(store_path);
            store.require()?;
            let errors = validate_graph(&store);
            if errors.is_empty() {
                println!("graph valid: {}", store.root.display());
                Ok(0)
            } else {
                println!("graph invalid: {} error(s)", errors.len());
                for e in &errors {
                    println!("  - {e}");
                }
                Ok(1)
            }
        }
        SubCommand::Frontier => {
            let store = Store::new(store_path);
            store.require()?;
            let fr = frontier(&store)?;
            if fr.is_empty() {
                println!("(no ready intents)");
                return Ok(0);
            }
            for i in &fr {
                let mark = if i["status"].as_str() == Some("claimed") {
                    format!("  [claimed: {}]", i["worker"].as_str().unwrap_or("?"))
                } else if i
                    .get("requires_confirmation")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                    && !i.get("confirmed_at").map(|v| !v.is_null()).unwrap_or(false)
                {
                    "  [needs confirmation]".to_string()
                } else {
                    String::new()
                };
                println!(
                    "{}\tfrom={}\t{}{mark}",
                    i["id"].as_str().unwrap_or("?"),
                    i["from"]
                        .as_array()
                        .map(|a| a
                            .iter()
                            .filter_map(|v| v.as_str())
                            .collect::<Vec<_>>()
                            .join(","))
                        .unwrap_or_default(),
                    i["description"].as_str().unwrap_or("")
                );
            }
            Ok(0)
        }
        SubCommand::Pick { claim, any, worker } => {
            let store = Store::new(store_path);
            store.require()?;
            let fr = frontier(&store)?;
            let worker: Option<String> = if *claim {
                Some(
                    worker
                        .clone()
                        .unwrap_or_else(|| crate::engine::worker_id("agent")),
                )
            } else {
                None
            };
            let mut pool: Vec<Value> = if fr.is_empty() {
                vec![]
            } else {
                fr.into_iter()
                    .filter(|i| {
                        if !claim {
                            return true;
                        }
                        let w = i.get("worker").and_then(|v| v.as_str()).unwrap_or("");
                        w.is_empty() || w == worker.as_deref().unwrap_or("")
                    })
                    .collect()
            };
            if pool.is_empty() && *any {
                pool = store.list_intents()?.into_iter().filter(is_open).collect();
            }
            if pool.is_empty() {
                println!("(nothing ready; pass --any to include unconfirmed/blocked open intents)");
                return Ok(0);
            }
            let chosen = pool[0].clone();
            let chosen = if *claim {
                let w = worker.as_ref().unwrap();
                store.claim_intent(chosen["id"].as_str().unwrap_or(""), w)?
            } else {
                chosen
            };
            println!("{}", chosen["id"].as_str().unwrap_or("?"));
            Ok(0)
        }
        SubCommand::Fact {
            description,
            title,
            kind,
            data,
            doc,
            doc_file,
            creator,
            from_intent,
            json,
        } => {
            let store = Store::new(store_path);
            store.require()?;
            let doc = read_doc_arg(doc.as_deref(), doc_file.as_deref())?;
            let data = parse_kv_pairs(data)?;
            let fact = store.add_fact(
                description,
                creator,
                from_intent.as_deref(),
                title.as_deref(),
                doc.as_deref(),
                kind.as_deref(),
                Some(&data),
            )?;
            println!("{}", fact["id"].as_str().unwrap_or("?"));
            if *json {
                println!("{}", dump_json(&fact));
            }
            Ok(0)
        }
        SubCommand::Intent {
            from,
            description,
            title,
            doc,
            doc_file,
            creator,
            confirm,
            json,
        } => {
            let store = Store::new(store_path);
            store.require()?;
            let from_ids = parse_from(from);
            let doc = read_doc_arg(doc.as_deref(), doc_file.as_deref())?;
            let intent = store.add_intent(
                from_ids,
                description,
                creator,
                *confirm,
                title.as_deref(),
                doc.as_deref(),
            )?;
            println!("{}", intent["id"].as_str().unwrap_or("?"));
            if *json {
                println!("{}", dump_json(&intent));
            }
            Ok(0)
        }
        SubCommand::Hint {
            content,
            creator,
            list,
        } => {
            let store = Store::new(store_path);
            store.require()?;
            if *list {
                for h in store.list_hints()? {
                    println!(
                        "{}\t({})\t{}",
                        h["id"].as_str().unwrap_or("?"),
                        h["creator"].as_str().unwrap_or("?"),
                        h["content"].as_str().unwrap_or("")
                    );
                }
                return Ok(0);
            }
            let content = content
                .as_deref()
                .ok_or_else(|| SapperError::new("hint content required (or use --list)"))?;
            let h = store.add_hint(content, creator)?;
            println!("{}", h["id"].as_str().unwrap_or("?"));
            Ok(0)
        }
        SubCommand::Peers {
            discover,
            add,
            remove,
            target,
            sender,
            channel,
            note,
            json,
        } => {
            let store = Store::new(store_path);
            store.require()?;
            peers::cmd_peers(
                &store,
                *discover,
                add.as_deref(),
                remove.as_deref(),
                target.as_deref(),
                sender.as_deref(),
                channel.as_deref(),
                note.as_deref(),
                *json,
            )
        }
        SubCommand::Send {
            peer,
            message,
            sender,
            channel,
            no_log,
            raw,
        } => {
            let store = Store::new(store_path);
            store.require()?;
            peers::cmd_send(
                &store,
                peer,
                message,
                sender.as_deref(),
                channel.as_deref(),
                *no_log,
                *raw,
            )
        }
        SubCommand::Capture {
            peer,
            lines,
            channel,
            log,
        } => {
            let store = Store::new(store_path);
            store.require()?;
            peers::cmd_capture(&store, peer, *lines, channel.as_deref(), *log)
        }
        SubCommand::Show { entity, id } => {
            let store = Store::new(store_path);
            store.require()?;
            if entity == "project" {
                println!("{}", dump_json(&store.load_project()?));
                return Ok(0);
            }
            let id = id
                .as_deref()
                .ok_or_else(|| SapperError::new(format!("show {entity} requires an id")))?;
            let v = if entity == "fact" {
                store.get_fact(id)?
            } else {
                store.get_intent(id)?
            };
            println!("{}", dump_json(&v));
            Ok(0)
        }
        SubCommand::Doc {
            id,
            set,
            set_file,
            edit,
        } => {
            let store = Store::new(store_path);
            store.require()?;
            crate::store::validate_lookup_id(id, "node")?;
            let node = if id == "goal" || store.facts_dir.join(format!("{id}.json")).exists() {
                store.get_fact(id)?
            } else if store.intents_dir.join(format!("{id}.json")).exists() {
                store.get_intent(id)?
            } else {
                return Err(SapperError::new(format!("no fact or intent named {id}")));
            };
            if set.is_some() || set_file.is_some() || *edit {
                let expected_doc = node.get("doc").cloned();
                let new_doc = if *edit {
                    Some(edit_via_editor(
                        node.get("doc").and_then(|v| v.as_str()).unwrap_or(""),
                        &format!("{id} doc"),
                    )?)
                } else if let Some(f) = set_file {
                    Some(std::fs::read_to_string(f).map_err(|e| {
                        SapperError::new(format!("cannot read doc file {f:?}: {e}"))
                    })?)
                } else {
                    set.clone()
                };
                let node = store.update_doc(id, new_doc.as_deref(), Some(&expected_doc))?;
                let doc_len = node
                    .get("doc")
                    .and_then(|v| v.as_str())
                    .map(|s| s.len())
                    .unwrap_or(0);
                if doc_len > 0 {
                    println!("updated doc for {id} ({doc_len} chars)");
                } else {
                    println!("cleared doc for {id}");
                }
                return Ok(0);
            }
            let doc = node.get("doc").and_then(|v| v.as_str());
            match doc {
                Some(d) if !d.is_empty() => {
                    println!("{d}");
                    Ok(0)
                }
                _ => {
                    eprintln!("{id} has no doc. add one: sapper doc {id} --set '...' or --edit");
                    Ok(1)
                }
            }
        }
        SubCommand::Recall {
            query,
            limit,
            kind,
            json,
        } => {
            let store = Store::new(store_path);
            store.require()?;
            if query.trim().is_empty() {
                return Err(SapperError::new("recall: query is required"));
            }
            if *limit < 1 {
                return Err(SapperError::new("recall: --limit must be >= 1"));
            }
            let kinds: Vec<&str> = match kind.as_str() {
                "fact" => vec!["fact"],
                "intent" => vec!["intent"],
                _ => vec!["fact", "intent"],
            };
            let matches = recall(&store, query, *limit as usize, &kinds)?;
            if *json {
                let vals: Vec<Value> = matches
                    .iter()
                    .map(|m| {
                        json!({
                            "id": m.id,
                            "kind": m.kind,
                            "title": m.title,
                            "description": m.description,
                            "has_doc": m.has_doc,
                            "score": m.score,
                        })
                    })
                    .collect();
                println!("{}", dump_json(&Value::Array(vals)));
            } else {
                println!("{}", render_recall(&matches, query));
            }
            Ok(if matches.is_empty() { 1 } else { 0 })
        }
        SubCommand::Claim { id, worker } => {
            let store = Store::new(store_path);
            store.require()?;
            let intent = store.claim_intent(id, worker)?;
            println!(
                "claimed {} -> {}",
                intent["id"].as_str().unwrap_or("?"),
                worker
            );
            Ok(0)
        }
        SubCommand::Release { id, worker } => {
            let store = Store::new(store_path);
            store.require()?;
            let worker = worker
                .clone()
                .unwrap_or_else(|| format!("agent-{}", std::process::id()));
            let intent = store.release_intent(id, &worker)?;
            println!("released {}", intent["id"].as_str().unwrap_or("?"));
            Ok(0)
        }
        SubCommand::Done {
            id,
            fact,
            title,
            kind,
            data,
            doc,
            doc_file,
            worker,
        } => {
            let store = Store::new(store_path);
            store.require()?;
            let doc = read_doc_arg(doc.as_deref(), doc_file.as_deref())?;
            let data = parse_kv_pairs(data)?;
            let (fact_v, intent) = store.conclude_with_new_fact(
                id,
                fact,
                worker,
                title.as_deref(),
                doc.as_deref(),
                kind.as_deref(),
                Some(&data),
                false,
            )?;
            println!(
                "{id} -> {}  ({})",
                fact_v["id"].as_str().unwrap_or("?"),
                intent["status"].as_str().unwrap_or("?")
            );
            Ok(0)
        }
        SubCommand::Complete { from, note, worker } => {
            let store = Store::new(store_path);
            store.require()?;
            let from_ids = parse_from(from);
            let note = note.clone().unwrap_or_else(|| "goal satisfied".into());
            let intent = store.complete_project(from_ids, &note, worker)?;
            println!(
                "project complete via {} (goal)",
                intent["id"].as_str().unwrap_or("?")
            );
            Ok(0)
        }
        SubCommand::Confirm { id } => {
            let store = Store::new(store_path);
            store.require()?;
            let intent = store.confirm_intent(id)?;
            println!("confirmed {}", intent["id"].as_str().unwrap_or("?"));
            Ok(0)
        }
        SubCommand::Dispatch {
            target,
            model,
            timeout,
            templates,
            skip_permissions,
            dry_run,
            claim,
            intent: verify_intent,
            max_options,
        } => {
            let store = Store::new(store_path);
            store.require()?;
            let cwd = std::env::current_dir()?;
            let templates_dir = effective_templates(templates.as_deref());
            let model = if model.trim().is_empty() {
                return Err(SapperError::new("--model cannot be empty"));
            } else {
                model.clone()
            };
            if *timeout < 1 {
                return Err(SapperError::new("--timeout must be at least 1 second"));
            }

            if target == "reason" {
                return dispatch_reason_printed(
                    &store,
                    &templates_dir,
                    &model,
                    *timeout,
                    *skip_permissions,
                    &cwd,
                    "",
                    *dry_run,
                );
            }
            if target == "verify" {
                let iid = verify_intent
                    .as_deref()
                    .ok_or_else(|| SapperError::new("verify requires --intent <id>"))?;
                let intent_v = store.get_intent(iid)?;
                if intent_v["to"].is_null() {
                    return Err(SapperError::new(format!("intent {iid} not concluded yet")));
                }
                let fact = store.get_fact(intent_v["to"].as_str().unwrap_or(""))?;
                let tpl = load_template(&templates_dir, "verify.md")?;
                let mut m: std::collections::HashMap<String, String> =
                    std::collections::HashMap::new();
                m.insert(
                    "intent_description".into(),
                    crate::util::scrub_inline(intent_v.get("description")),
                );
                m.insert(
                    "result_description".into(),
                    crate::util::scrub_inline(fact.get("description")),
                );
                let prompt = crate::graph::replace_placeholders(&tpl, &m);
                if *dry_run {
                    println!("{prompt}");
                    return Ok(0);
                }
                let envelope = crate::engine::run_engine(
                    &prompt,
                    &model,
                    *timeout,
                    *skip_permissions,
                    None,
                    &cwd,
                )?;
                let text = envelope
                    .get("result")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let payload = extract_json_object(text)?;
                let (verified, issues) = parse_verify_payload(&payload)?;
                let tag = if verified { "VERIFIED" } else { "FAILED" };
                println!(
                    "[verify] {iid}: {tag}{}",
                    if issues.is_empty() {
                        String::new()
                    } else {
                        format!(" — {issues}")
                    }
                );
                return Ok(if verified { 0 } else { 1 });
            }
            if target == "blindspot" {
                let tpl = load_template(&templates_dir, "blindspot.md")?;
                let prompt = crate::graph::render_prompt(&store, &tpl, "")?;
                if *dry_run {
                    println!("{prompt}");
                    return Ok(0);
                }
                let envelope = crate::engine::run_engine(
                    &prompt,
                    &model,
                    *timeout,
                    *skip_permissions,
                    None,
                    &cwd,
                )?;
                let text = envelope
                    .get("result")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let payload = extract_json_object(text)?;
                let summary = apply_blindspot(&store, &payload)?;
                println!("[blindspot] {summary}");
                return Ok(0);
            }
            if target == "brainstorm" {
                if *max_options < 1 || *max_options > 20 {
                    return Err(SapperError::new("--max-options must be between 1 and 20"));
                }
                let mut tpl = load_template(&templates_dir, "brainstorm.md")?;
                let max_options_s = max_options.to_string();
                // Fill this template-only token before graph content is inserted.
                tpl = tpl.replace("{max_options}", &max_options_s);
                let prompt = crate::graph::render_prompt(&store, &tpl, "")?;
                if *dry_run {
                    println!("{prompt}");
                    return Ok(0);
                }
                let envelope = crate::engine::run_engine(
                    &prompt,
                    &model,
                    *timeout,
                    *skip_permissions,
                    None,
                    &cwd,
                )?;
                let text = envelope
                    .get("result")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let payload = extract_json_object(text)?;
                let summary = apply_brainstorm(&store, &payload, Some(*max_options as usize))?;
                println!("[brainstorm] {summary}");
                return Ok(0);
            }
            // treat as intent id -> explore
            let intent_v = store.get_intent(target)?;
            if !intent_v["to"].is_null() {
                let fact = store.get_fact(intent_v["to"].as_str().unwrap_or(""))?;
                println!(
                    "[explore] {} already concluded -> {}: {}",
                    intent_v["id"].as_str().unwrap_or("?"),
                    fact["id"].as_str().unwrap_or("?"),
                    fact["description"].as_str().unwrap_or("")
                );
                return Ok(0);
            }
            if !*dry_run {
                let needs = intent_v
                    .get("requires_confirmation")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let confirmed = intent_v
                    .get("confirmed_at")
                    .map(|v| !v.is_null())
                    .unwrap_or(false);
                if needs && !confirmed {
                    return Err(SapperError::new(format!(
                        "intent {} requires confirmation; run `sapper confirm {}` first",
                        target, target
                    )));
                }
            }
            if !*skip_permissions {
                eprintln!(
                    "[warn] explore without --skip-permissions will prompt for every tool use; pass --skip-permissions for autonomous runs."
                );
            }
            let worker = crate::engine::worker_id(&model);
            let tpl = load_template(&templates_dir, "explore.md")?;
            let prompt = crate::graph::render_intent_prompt(&store, &tpl, &intent_v)?;
            if *dry_run {
                println!("{prompt}");
                return Ok(0);
            }
            if *claim {
                store.claim_intent(target, &worker)?;
            }
            let envelope = crate::engine::run_engine(
                &prompt,
                &model,
                *timeout,
                *skip_permissions,
                Some(&crate::dispatch::agents_system_prompt(&store.root)),
                &cwd,
            )?;
            let text = envelope
                .get("result")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let payload = extract_json_object(text)?;
            let (title, desc, doc, kind, fdata) = parse_explore_payload(&payload)?;
            let (fact_v, intent_v2) = store.conclude_with_new_fact(
                target,
                &desc,
                &worker,
                title.as_deref(),
                doc.as_deref(),
                kind.as_deref(),
                fdata.as_ref(),
                false,
            )?;
            println!(
                "[explore] {} -> {}: {}",
                intent_v2["id"].as_str().unwrap_or("?"),
                fact_v["id"].as_str().unwrap_or("?"),
                desc
            );
            Ok(0)
        }
        SubCommand::BrainstormPick { id } => {
            let store = Store::new(store_path);
            store.require()?;
            let msg = brainstorm_pick(&store, id)?;
            println!("{msg}");
            Ok(0)
        }
        SubCommand::Setup {
            goal,
            origin,
            agents,
            force,
        } => cmd_setup(store_path, goal, origin, *agents, *force),
        SubCommand::Teardown { purge } => cmd_teardown(store_path, *purge, cli),
        SubCommand::View {
            out,
            serve,
            host,
            port,
            print: print_only,
        } => {
            let store = Store::new(store_path);
            view::cmd_view(&store, out.as_deref(), *serve, host, *port, *print_only)
        }
        SubCommand::Flag {
            value,
            challenge,
            submit,
            title,
            creator,
        } => cmd_flag(
            store_path,
            value,
            challenge.as_deref(),
            *submit,
            title.as_deref(),
            creator,
        ),
        SubCommand::Arena {
            action,
            challenge_id,
            flag,
        } => cmd_arena(store_path, action, challenge_id.as_deref(), flag.as_deref()),
        SubCommand::Kb {
            action,
            target,
            description,
            detail,
            detail_file,
            tag,
            command,
            source,
            path: _path,
            root,
            limit,
        } => cmd_kb(
            action,
            target.as_deref(),
            description.as_deref(),
            detail.as_deref(),
            detail_file.as_deref(),
            tag,
            command,
            source.as_deref(),
            root.as_deref(),
            *limit as usize,
        ),
        SubCommand::Bench {
            root,
            minutes,
            deadline,
            minutes_per_challenge,
            max_steps,
            round_gap_seconds,
            parallel,
            poll_seconds,
            model,
            templates,
            skip_permissions,
            verify,
        } => {
            let _ = effective_templates;
            let _ = cli;
            cmd_bench(BenchOpts {
                root: root.clone(),
                minutes: *minutes,
                deadline: deadline.clone(),
                minutes_per_challenge: *minutes_per_challenge,
                max_steps: *max_steps,
                round_gap_seconds: *round_gap_seconds,
                parallel: *parallel,
                poll_seconds: *poll_seconds,
                model: model.clone(),
                templates: templates.clone(),
                skip_permissions: *skip_permissions,
                verify: *verify,
            })
        }
        SubCommand::Command {
            root,
            minutes,
            deadline,
            minutes_default,
            minutes_max,
            fleet_file,
            poll_seconds,
            model,
            timeout,
            templates,
            skip_permissions,
            verify,
            max_steps,
        } => cmd_command(&CommandOpts {
            root: root.clone(),
            minutes: *minutes,
            deadline: deadline.clone(),
            minutes_default: *minutes_default,
            minutes_max: *minutes_max,
            fleet_file: fleet_file.clone(),
            poll_seconds: *poll_seconds,
            model: model.clone(),
            timeout: *timeout,
            templates: templates.clone(),
            skip_permissions: *skip_permissions,
            verify: *verify,
            max_steps: *max_steps,
        }),
        SubCommand::Fleet {
            action,
            file,
            target,
            minutes,
            max_steps,
            extra_args,
        } => cmd_fleet(FleetArgs {
            action: action.clone(),
            file: file.clone(),
            target: target.clone(),
            minutes: *minutes,
            max_steps: *max_steps,
            extra_args: extra_args.clone(),
        }),
        SubCommand::Sweep {
            root,
            minutes,
            deadline,
            minutes_per,
            model,
            templates,
            skip_permissions,
            verify,
            max_steps,
        } => {
            let _ = (templates, skip_permissions, verify);
            cmd_sweep(&SweepOpts {
                root: root.clone(),
                minutes: *minutes,
                deadline: deadline.clone(),
                minutes_per: *minutes_per,
                model: model.clone(),
                templates: templates.clone(),
                skip_permissions: *skip_permissions,
                verify: *verify,
                max_steps: *max_steps,
            })
        }
        SubCommand::Auto {
            model,
            timeout,
            templates,
            skip_permissions,
            dry_run,
            verify,
            max_steps,
            max_explore,
            minutes,
            deadline,
        } => {
            let store = Store::new(store_path);
            cmd_auto(AutoOpts {
                store,
                model: model.clone(),
                timeout: *timeout,
                templates: effective_templates(templates.as_deref()),
                skip_permissions: *skip_permissions,
                dry_run: *dry_run,
                verify: *verify,
                max_steps: *max_steps,
                max_explore: *max_explore,
                minutes: *minutes,
                deadline: deadline.clone(),
            })
        }
        SubCommand::Hook { event } => crate::hooks::cmd_hook(event),
    }
}

fn read_doc_arg(doc: Option<&str>, doc_file: Option<&str>) -> Result<Option<String>> {
    if let Some(f) = doc_file {
        let p = Path::new(f);
        if !p.exists() {
            return Err(SapperError::new(format!("doc file not found: {f}")));
        }
        let text = std::fs::read_to_string(p)?;
        return Ok(Some(text));
    }
    Ok(doc.map(|s| s.to_string()))
}

fn cmd_init(store_path: &Path, origin: &str, goal: &str, force: bool) -> Result<i32> {
    let store = Store::new(store_path);
    if store.exists() && !force {
        return Err(SapperError::new(format!(
            "graph exists at {}; use --force to overwrite",
            store.root.display()
        )));
    }
    if force && store.root.exists() {
        assert_safe_store_root(&store.root, "overwrite")?;
        std::fs::remove_dir_all(&store.root)?;
    }
    if goal.trim().is_empty() {
        return Err(SapperError::new("--goal is required"));
    }
    store.init(origin, goal)?;
    println!("initialized fact graph at {}", store.root.display());
    println!("goal: {goal}");
    Ok(0)
}

fn cmd_setup(
    store_path: &Path,
    goal: &Option<String>,
    origin: &str,
    agents: bool,
    force: bool,
) -> Result<i32> {
    let project_dir = std::env::current_dir()?;
    let store = Store::new(store_path);
    let target = settings::project_settings_path(&project_dir);
    if target.exists() {
        settings::validate(&target)?;
    }
    if !store.exists() {
        let goal = goal
            .as_deref()
            .ok_or_else(|| {
                SapperError::new(
                    "no .sapper/ here. Pass --goal \"<one-line goal>\" to create one, or run from a directory that already has a graph.",
                )
            })?;
        store.init(origin, goal)?;
        println!("created graph at {} (goal: {goal})", store.root.display());
    } else {
        println!("graph already present at {}", store.root.display());
    }
    let (_, msgs) = settings::add(&target, "")?;
    for m in msgs {
        println!("{m}");
    }
    if agents {
        let ap = project_dir.join("AGENTS.md");
        if ap.exists() && !force {
            println!("AGENTS.md exists — leaving it (--force to overwrite)");
        } else {
            let spec = agents_spec(&store.root);
            std::fs::write(&ap, spec)?;
            println!("wrote {}", ap.display());
        }
    }
    println!(
        "\nsetup done. This project now auto-reads/maintains its fact graph.\n\
         \"
  sapper status          # see current state
  sapper dispatch reason # propose next work
  sapper peers --discover # optional: list tmux panes, then ask user who to authorize
  sapper auto --skip-permissions  # drive to completion
\"
To undo:  sapper teardown   (removes sapper hooks + its AGENTS.md; keeps .sapper/)"
    );
    Ok(0)
}

const AGENTS_MARKER: &str =
    "<!-- generated by sapper; edit or remove this marker to keep this file -->";

fn agents_spec(store_root: &Path) -> String {
    format!(
        "{AGENTS_MARKER}
# AGENTS.md — fact-graph protocol

This project uses a fact-graph as shared working memory. The graph lives at
`{store}` and is manipulated by the `sapper` CLI.

## model
- **fact**: a confirmed observation or reproducible result. Immutable once written.
- **intent**: a unit of work. `from` = facts it depends on; `to` = the fact it
  produced (null while open). An intent with `to == \"goal\"` completes the project.
- **hint**: a live human message to the commander.

## commands you will use
```
sapper status                     # project state + what's ready to work
sapper graph                      # full graph as text (title + one-line summary + 📄 markers)
sapper frontier                   # only the ready open intents
sapper show intent <id>           # full node detail
sapper doc <id>                   # read the DETAILED document of a fact/intent (when you need depth)
sapper fact \"<gist>\" -t \"<title>\" [--doc \"<detail>\"]   # record a confirmed result
sapper fact \"<gist>\" --from-intent i003              # record + close i003 atomically
sapper done <intent-id> --fact \"<gist>\" -t \"<title>\"   # finish an intent, produce a fact
sapper hint \"<message>\"           # leave a note for the commander / human
sapper peers --discover           # list tmux panes before asking the user to authorize peers
sapper peers                      # list user-authorized peer targets from .sapper/ai-peers.json
sapper send <peer> \"<message>\"    # message an authorized peer via tmux + append .sapper/ai-channel.txt
sapper capture <peer>             # read recent output from an authorized peer pane
```

## three documentation layers (every node)
1. **title** — short (2-10 字) human label, shown in the graph view.
2. **description** — one-sentence gist. Injected into prompts by default.
3. **doc** — detailed markdown (commands, paths, evidence). NOT in prompts by
   default; READ it with `sapper doc <id>` when you need the full context.
   WRITE it with `--doc \"...\"` or `--doc-file <path>`.

## rules
1. Record only what you actually confirmed with a command, file, or log.
2. Never invent results. If evidence is missing, say so in the fact description.
3. Stay scoped to your assigned intent unless a blocker forces otherwise.
4. Prefer reproducible evidence: exact commands, paths, versions, error messages.
5. Conclude your intent with `sapper done` so the commander can pick the next step.
6. Never send messages to a tmux pane unless the user explicitly authorized it
   and it is listed by `sapper peers` in `{store}/ai-peers.json`.
7. For authorized peer communication, prefer `sapper send <peer> ...`; it uses the
   reliable paste-buffer newline flow and records the message in the shared log.
8. After sending a question, proactively check for replies with
   `sapper capture <peer> --lines 120`; peers may answer directly in their TUI, so
   not every reply appears in ai-channel.txt.
",
        store = store_root.display()
    )
}

fn cmd_teardown(store_path: &Path, purge: bool, cli: &Cli) -> Result<i32> {
    let project_dir = std::env::current_dir()?;
    let target = settings::project_settings_path(&project_dir);
    if target.exists() {
        let (_, msgs) = settings::remove(&target, "")?;
        for m in &msgs {
            println!("{m}");
        }
        // if the project settings.json is now empty, drop it (and its dir)
        let leftover: Value = match std::fs::read_to_string(&target) {
            Ok(raw) if !raw.trim().is_empty() => {
                serde_json::from_str(raw.trim()).unwrap_or(Value::Null)
            }
            _ => Value::Null,
        };
        if leftover == json!({}) {
            let _ = std::fs::remove_file(&target);
            println!("removed empty {target:?}");
            let parent = target.parent().unwrap();
            if parent.exists() {
                let empty = std::fs::read_dir(parent)
                    .map(|it| it.count() == 0)
                    .unwrap_or(false);
                if empty {
                    let _ = std::fs::remove_dir(parent);
                    println!("removed empty {parent:?}");
                }
            }
        }
    }
    let ap = project_dir.join("AGENTS.md");
    if ap.exists() {
        let text = std::fs::read_to_string(&ap).unwrap_or_default();
        let generated = text.starts_with(AGENTS_MARKER)
            || text
                == agents_spec(
                    &PathBuf::from(store_path)
                        .canonicalize()
                        .unwrap_or_else(|_| PathBuf::from(cli.store.clone())),
                );
        if generated {
            std::fs::remove_file(&ap)?;
            println!("removed {}", ap.display());
        } else {
            println!("kept existing {} (not generated by sapper)", ap.display());
        }
    }
    if purge {
        let store_root = PathBuf::from(cli.store.clone());
        if store_root.exists() {
            assert_safe_store_root(&store_root, "purge")?;
            std::fs::remove_dir_all(&store_root)?;
            println!("purged graph data {}", store_root.display());
        }
    } else {
        let sr = PathBuf::from(cli.store.clone());
        if sr.exists() {
            println!(
                "(kept graph data at {}; pass --purge to delete it too)",
                sr.display()
            );
        }
    }
    Ok(0)
}

fn find_flag_fact(store: &Store, value: &str) -> Option<Value> {
    store.list_facts().ok()?.into_iter().find(|f| {
        f.get("kind").and_then(|v| v.as_str()) == Some("flag")
            && f.get("data")
                .and_then(|d| d.get("value"))
                .and_then(|v| v.as_str())
                == Some(value)
    })
}

fn cmd_flag(
    store_path: &Path,
    value: &str,
    challenge: Option<&str>,
    submit: bool,
    title: Option<&str>,
    creator: &str,
) -> Result<i32> {
    let store = Store::new(store_path);
    store.require()?;
    let value = value.trim();
    if value.is_empty() {
        return Err(SapperError::new("flag value cannot be empty"));
    }
    let existing = find_flag_fact(&store, value);
    if let Some(existing) = existing {
        let d = existing.get("data").cloned().unwrap_or(Value::Null);
        let d = if d.is_object() {
            &d
        } else {
            &Value::Object(serde_json::Map::new())
        };
        return Err(SapperError::new(format!(
            "flag already recorded as {} (status={}, challenge={})",
            existing["id"].as_str().unwrap_or("?"),
            d.get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("recorded"),
            d.get("challenge").and_then(|v| v.as_str()).unwrap_or("?")
        )));
    }
    let challenge = (challenge.unwrap_or("")).trim();
    let description = if challenge.is_empty() {
        format!("flag captured: {value}")
    } else {
        format!("flag captured: {value} (challenge {challenge})")
    };
    let data = normalize_fact_data(Some(
        &serde_json::json!({
            "value": value,
            "challenge": challenge,
            "status": "recorded",
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    ))?;
    let flag_data = data.as_object().cloned().unwrap_or_default();
    let fact = store.add_fact(
        &description,
        creator,
        None,
        title,
        None,
        Some("flag"),
        Some(&flag_data),
    )?;
    println!("{} recorded", fact["id"].as_str().unwrap_or("?"));
    if !submit {
        println!("(not submitted; pass --submit to send it via the arena API)");
        return Ok(0);
    }
    let client = ArenaClient::load(Some(&store.root))?;
    if challenge.is_empty() {
        eprintln!("sapper: warning: no --challenge given; submit endpoint may need it");
    }
    let reply = client.submit_flag(if challenge.is_empty() { "-" } else { challenge }, value)?;
    let raw = serde_json::to_string(&reply.get("result").cloned().unwrap_or_else(|| json!({})))
        .unwrap_or_default();
    let accepted = reply.get("status").and_then(|v| v.as_i64()) == Some(200);
    let status = if accepted { "submitted" } else { "rejected" };
    let fid = fact["id"].as_str().unwrap_or("").to_string();
    store.update_flag_fact(
        &fid,
        status,
        Some(&raw.chars().take(500).collect::<String>()),
    )?;
    println!(
        "{} {}: {}",
        fact["id"].as_str().unwrap_or("?"),
        status,
        raw.chars().take(300).collect::<String>()
    );
    Ok(if accepted { 0 } else { 1 })
}

fn cmd_arena(
    store_path: &Path,
    action: &str,
    challenge_id: Option<&str>,
    flag: Option<&str>,
) -> Result<i32> {
    let store = Store::new(store_path);
    if action == "init" {
        let path = ArenaClient::config_path(Some(&store.root));
        if path.exists() {
            return Err(SapperError::new(format!(
                "arena config already exists: {path:?}"
            )));
        }
        let written = ArenaClient::write_template(&store.root)?;
        println!(
            "wrote {} — fill in the official API base_url/endpoints, \
             then set BENCHMARK_TOKEN",
            written.display()
        );
        return Ok(0);
    }
    store.require()?;
    let client = ArenaClient::load(Some(&store.root))?;
    let reply = match action {
        "challenges" => client.list_challenges().map(|v| json!({ "result": v }))?,
        "start" => {
            let id =
                challenge_id.ok_or_else(|| SapperError::new("arena start needs a challenge_id"))?;
            json!({ "result": client.start_challenge(id)? })
        }
        "stop" => {
            let id =
                challenge_id.ok_or_else(|| SapperError::new("arena stop needs a challenge_id"))?;
            json!({ "result": client.close_challenge(id)? })
        }
        "submit" => {
            let id = challenge_id
                .ok_or_else(|| SapperError::new("arena submit needs a challenge_id"))?;
            let f = flag.ok_or_else(|| SapperError::new("arena submit needs --flag"))?;
            json!({ "result": client.submit_flag(id, f)? })
        }
        "hint" => {
            eprintln!(
                "sapper: note — official rules deduct score for hints (-10%); only ask when genuinely stuck"
            );
            let id =
                challenge_id.ok_or_else(|| SapperError::new("arena hint needs a challenge_id"))?;
            json!({ "result": client.get_hint(id)? })
        }
        _ => {
            return Err(SapperError::new(format!("unknown arena action {action:?}")));
        }
    };
    println!("{}", dump_json(&reply["result"]));
    Ok(0)
}

fn cmd_kb(
    action: &str,
    target: Option<&str>,
    description: Option<&str>,
    detail: Option<&str>,
    detail_file: Option<&str>,
    tag: &[String],
    command: &[String],
    source: Option<&str>,
    root: Option<&str>,
    limit: usize,
) -> Result<i32> {
    let kb_root: PathBuf = if let Some(r) = root {
        expand_user(r)
    } else {
        KnowledgeBase::default_root()
    };
    let kb = KnowledgeBase::new(kb_root);
    match action {
        "add" => {
            let mut detail = detail.unwrap_or("").to_string();
            if let Some(f) = detail_file {
                detail = std::fs::read_to_string(f)
                    .map_err(|e| SapperError::new(format!("cannot read detail file {f:?}: {e}")))?;
            }
            let entry = kb.add(
                target.unwrap_or(""),
                description.unwrap_or(""),
                &detail,
                command,
                tag,
                source.unwrap_or(""),
                None,
            )?;
            println!(
                "{}  {}",
                entry["id"].as_str().unwrap_or("?"),
                entry["title"].as_str().unwrap_or("")
            );
            Ok(0)
        }
        "show" => {
            let e = kb.get(target.unwrap_or("kb???"))?;
            println!("{}", dump_json(&e));
            Ok(0)
        }
        "list" => {
            let mut entries = kb.list_entries();
            if !tag.is_empty() {
                let wants: std::collections::HashSet<String> =
                    tag.iter().map(|t| t.trim().to_lowercase()).collect();
                entries.retain(|e| {
                    let etags: Vec<String> = e
                        .get("tags")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_lowercase()))
                                .collect()
                        })
                        .unwrap_or_default();
                    etags.iter().any(|t| wants.contains(t))
                });
            }
            if entries.is_empty() {
                println!(
                    "(no kb entries matching tag filter at {})",
                    kb.root.display()
                );
                return Ok(0);
            }
            for e in entries {
                let tags: Vec<String> = e
                    .get("tags")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                println!(
                    "{}\t[{}]\t{}",
                    e["id"].as_str().unwrap_or("?"),
                    tags.join(","),
                    e["title"].as_str().unwrap_or("")
                );
            }
            Ok(0)
        }
        "search" => {
            let t = tag.first().map(|s| s.as_str());
            let hits = kb.search(target.unwrap_or(""), t, limit)?;
            if hits.is_empty() {
                println!("(no kb matches — try other keywords, `sapper kb list` to browse)");
                return Ok(0);
            }
            for h in hits {
                let tags: Vec<String> = h
                    .get("tags")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                println!(
                    "{}\tscore={}\t[{}]\t{}",
                    h["id"].as_str().unwrap_or("?"),
                    h["score"],
                    tags.join(","),
                    h["title"].as_str().unwrap_or("")
                );
                if let Some(d) = h.get("description").and_then(|v| v.as_str()) {
                    if !d.is_empty() {
                        println!("      {}", d.chars().take(100).collect::<String>());
                    }
                }
            }
            println!("read full entries with: sapper kb show <id>");
            Ok(0)
        }
        "import" => {
            let target_path = PathBuf::from(target.unwrap_or(""));
            if !target_path.exists() {
                return Err(SapperError::new(format!(
                    "import path not found: {target_path:?}"
                )));
            }
            let files: Vec<PathBuf> = if target_path.is_dir() {
                let mut v: Vec<PathBuf> = target_path
                    .read_dir()
                    .map(|it| {
                        it.flatten()
                            .map(|e| e.path())
                            .filter(|p| {
                                p.extension()
                                    .map(|e| e.to_string_lossy() == "md")
                                    .unwrap_or(false)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                v.sort();
                if v.is_empty() {
                    return Err(SapperError::new(format!(
                        "no .md files under {target_path:?}"
                    )));
                }
                v
            } else {
                vec![target_path.clone()]
            };
            let mut added = 0;
            for f in &files {
                let parsed = import_markdown(f, tag, &f.to_string_lossy());
                let entry = kb.add(
                    parsed.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                    parsed
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or(""),
                    parsed.get("detail").and_then(|v| v.as_str()).unwrap_or(""),
                    &[],
                    tag,
                    parsed.get("source").and_then(|v| v.as_str()).unwrap_or(""),
                    None,
                )?;
                println!(
                    "{}  {}",
                    entry["id"].as_str().unwrap_or("?"),
                    entry["title"].as_str().unwrap_or("")
                );
                added += 1;
            }
            println!(
                "imported {added} entr{} into {}",
                if added == 1 { "y" } else { "ies" },
                kb.root.display()
            );
            Ok(0)
        }
        "root" => {
            println!("{}", kb.root.display());
            Ok(0)
        }
        other => Err(SapperError::new(format!("unknown kb action {other:?}"))),
    }
}

#[allow(dead_code)]
fn _unused_imports() {
    let _ = (
        deadline_from_args,
        now_local_stamp,
        dump_json,
        graph_signature,
        is_ready,
        expand_user,
    );
}
