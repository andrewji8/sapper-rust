//! HTML visualization of the graph: static snapshot file or a live
//! auto-refreshing HTTP server. Port of the Python `view` commands.

use crate::error::{Result, SapperError};
use crate::graph::{build_graph_view, graph_view_json};
use crate::store::Store;
use serde_json::Value;
use std::path::{Path, PathBuf};

const VIEW_TEMPLATE: &str = include_str!("../assets/view_template.html");

/// Locate the bundled `templates/` and `kb/` assets: next to the running
/// executable, else in the CWD.
pub fn resolve_templates_dir(cli: Option<&str>, store_root: &Path) -> PathBuf {
    if let Some(t) = cli {
        return PathBuf::from(t);
    }
    // default: sibling `templates/` next to the store, else a packaged
    // templates/ or assets/templates/ found by walking up from the exe.
    if let Some(parent) = store_root.parent() {
        let cand = parent.join("templates");
        if cand.is_dir() {
            return cand;
        }
    }
    if let Some(d) = crate::util::find_ancestor_asset(std::path::Path::new("assets/templates")) {
        if d.is_dir() {
            return d;
        }
    }
    if let Some(d) = crate::util::find_ancestor_asset(std::path::Path::new("templates")) {
        if d.is_dir() {
            return d;
        }
    }
    PathBuf::from("templates")
}

#[allow(dead_code)]
pub fn resolve_kb_root(cli: Option<&str>) -> PathBuf {
    if let Some(r) = cli {
        return PathBuf::from(r);
    }
    if let Ok(env) = std::env::var("SAPPER_KB_ROOT") {
        if !env.is_empty() {
            return crate::util::expand_user(&env);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            let cand = d.join("kb");
            if cand.join("entries").is_dir() {
                return cand;
            }
        }
    }
    std::env::current_dir()
        .map(|d| d.join("kb"))
        .unwrap_or_else(|_| PathBuf::from("kb"))
}

/// Inline the current graph into the HTML template as a snapshot.
pub fn render_static_html(store: &Store) -> Result<String> {
    let g = build_graph_view(store)?;
    let payload = Value::Object(graph_view_json(&g).as_object().cloned().unwrap_or_default());
    // JSON escaping alone does not prevent a fact containing `</script>`
    // from terminating that script before the browser parses it.
    let mut json_str = serde_json::to_string_pretty(&payload).unwrap_or_default();
    json_str = json_str
        .replace('&', "\\u0026")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");
    Ok(VIEW_TEMPLATE.replace(
        "/*__GRAPH_JSON__*/ null /*__END__*/",
        &format!("/*__GRAPH_JSON__*/ {json_str} /*__END__*/"),
    ))
}

pub fn cmd_view(
    store: &Store,
    out: Option<&str>,
    serve: bool,
    host: &str,
    port: u16,
    print_only: bool,
) -> Result<i32> {
    store.require()?;
    if serve {
        return view_serve(store, host, port);
    }
    if print_only {
        print!("{}", render_static_html(store)?);
        return Ok(0);
    }
    let out = match out {
        Some(o) => PathBuf::from(o),
        None => store
            .root
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("sapper-graph.html"),
    };
    std::fs::write(&out, render_static_html(store)?)?;
    let opened = crate::util::try_open(&out.to_string_lossy());
    println!("wrote {}", out.display());
    if opened {
        println!("(opened in browser)");
    } else {
        if let Ok(abs) = out.canonicalize() {
            println!(
                "open it: file:///{}",
                abs.to_string_lossy().replace('\\', "/")
            );
        }
    }
    Ok(0)
}

/// Serve a live HTML view + a JSON poll endpoint.
fn view_serve(store: &Store, host: &str, port: u16) -> Result<i32> {
    use tiny_http::{Header, Response, Server, StatusCode};
    let token = crate::util::random_hex(12);
    let poll_path = format!("/__sapper_graph__/{token}.json");
    let tpl = VIEW_TEMPLATE.to_string();

    let server = Server::http(format!("{host}:{port}"))
        .map_err(|e| SapperError::new(format!("could not bind {host}:{port}: {e}")))?;
    let base = store;
    let poll = poll_path.clone();
    let tpl_owned = tpl;

    println!("[view] serving fact-graph  →  http://{host}:{port}/");
    println!("[view] polling JSON at     →  http://{host}:{port}{poll}");
    if !matches!(host, "127.0.0.1" | "localhost" | "::1") {
        println!(
            "[view] WARNING: bound to a non-loopback host — the graph is readable by anyone who reaches this port."
        );
    }
    println!("[view] the page auto-refreshes every 2s. Ctrl-C to stop.");

    let host_owned = host.to_string();
    let _ = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(400));
        let _ = crate::util::try_open(&format!("http://{host_owned}:{port}/"));
    });

    use std::io::Cursor;
    for request in server.incoming_requests() {
        let url = request.url().split('?').next().unwrap_or("/").to_string();
        if url == poll {
            let g = build_graph_view(base).ok();
            match g {
                Some(gv) => {
                    let v = graph_view_json(&gv);
                    let bytes = serde_json::to_string(&v).unwrap_or_default().into_bytes();
                    let resp = Response::new(
                        StatusCode(200),
                        json_headers(),
                        Cursor::new(bytes),
                        None,
                        None,
                    );
                    let _ = request.respond(resp);
                }
                None => {
                    let body = serde_json::json!({"error": "graph read failed"})
                        .to_string()
                        .into_bytes();
                    let resp =
                        Response::new(StatusCode(500), Vec::new(), Cursor::new(body), None, None);
                    let _ = request.respond(resp);
                }
            }
        } else if url == "/" || url == "/index.html" {
            let html = tpl_owned.replace("\"__POLL_URL__\"", &format!("\"{poll}\""));
            let bytes = html.into_bytes();
            let mut headers: Vec<Header> = Vec::new();
            if let Ok(h) = Header::from_bytes(b"Content-Type", b"text/html; charset=utf-8") {
                headers.push(h);
            }
            let resp = Response::new(StatusCode(200), headers, Cursor::new(bytes), None, None);
            let _ = request.respond(resp);
        } else {
            let resp = Response::new(
                StatusCode(404),
                Vec::new(),
                Cursor::new(Vec::new()),
                None,
                None,
            );
            let _ = request.respond(resp);
        }
    }
    println!("\n[view] stopped.");
    Ok(0)
}

fn json_headers() -> Vec<tiny_http::Header> {
    let mut v: Vec<tiny_http::Header> = Vec::new();
    if let Ok(h) =
        tiny_http::Header::from_bytes(b"Content-Type", b"application/json; charset=utf-8")
    {
        v.push(h);
    }
    if let Ok(h) = tiny_http::Header::from_bytes(b"Cache-Control", b"no-store") {
        v.push(h);
    }
    v
}
