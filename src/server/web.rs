use crate::domain::model::DomainModel;
use crate::mcp::protocol::{JsonRpcRequest, JsonRpcResponse};
use crate::store::CrateRegistry;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::time::timeout;
use tracing::{info, warn};

pub const DEFAULT_WEB_PORT: u16 = 8888;

/// How long a single connection may take to deliver its request headers and body.
/// Without this a stalled peer pins a task (and its buffers) indefinitely.
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(15);

/// Upper bound on connections being served at once, so a flood of idle sockets
/// cannot exhaust memory or task slots.
const MAX_CONCURRENT_CONNECTIONS: usize = 64;

pub async fn run(registry: Arc<CrateRegistry>, preferred_port: u16) -> Result<()> {
    let listener = bind_localhost(preferred_port).await?;
    let addr = listener.local_addr()?;
    let port = addr.port();
    info!("Axon web graph available at http://{}", addr);
    let limit = Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTIONS));

    loop {
        let (stream, _) = listener.accept().await?;
        let Ok(permit) = Arc::clone(&limit).acquire_owned().await else {
            break Ok(());
        };
        let registry = Arc::clone(&registry);
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) = handle_connection(stream, registry, port).await {
                warn!("web request failed: {e:#}");
            }
        });
    }
}

async fn bind_localhost(port: u16) -> Result<TcpListener> {
    TcpListener::bind(("127.0.0.1", port))
        .await
        .with_context(|| format!("bind web server on 127.0.0.1:{port}"))
}

/// Multi-workspace web graph for the daemon: one server serving every registered
/// workspace, selected via `?workspace=<canonical-root>`.
pub async fn run_multi(registries: super::WorkspaceRegistries, preferred_port: u16) -> Result<()> {
    let listener = bind_localhost(preferred_port).await?;
    let addr = listener.local_addr()?;
    let port = addr.port();
    info!(
        "Axon multi-workspace web graph available at http://{}",
        addr
    );
    let limit = Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTIONS));

    loop {
        let (stream, _) = listener.accept().await?;
        let Ok(permit) = Arc::clone(&limit).acquire_owned().await else {
            break Ok(());
        };
        let registries = registries.clone();
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) = handle_connection_multi(stream, registries, port).await {
                warn!("web request failed: {e:#}");
            }
        });
    }
}

/// Read a request under a deadline, then apply the Host/Origin guard.
///
/// Returns `None` when the connection produced nothing or was already answered
/// with a rejection, so callers can simply stop.
async fn accept_request(stream: &mut TcpStream, port: u16) -> Result<Option<GuardedRequest>> {
    let request = match timeout(REQUEST_READ_TIMEOUT, read_http_request(stream)).await {
        Ok(result) => result?,
        Err(_) => bail!("HTTP request timed out after {REQUEST_READ_TIMEOUT:?}"),
    };
    let Some(request) = request else {
        return Ok(None);
    };

    match authorize(&request, port) {
        Ok(allow_origin) => Ok(Some(GuardedRequest {
            request,
            allow_origin,
        })),
        Err(reason) => {
            warn!("{reason}");
            // Answer without CORS headers: the browser will block the read, and a
            // non-browser caller still gets a clear status.
            respond(
                stream,
                "403 Forbidden",
                "text/plain; charset=utf-8",
                "forbidden",
                None,
            )
            .await?;
            Ok(None)
        }
    }
}

struct GuardedRequest {
    request: HttpRequest,
    allow_origin: Option<String>,
}

async fn handle_connection_multi(
    mut stream: TcpStream,
    registries: super::WorkspaceRegistries,
    port: u16,
) -> Result<()> {
    let Some(GuardedRequest {
        request,
        allow_origin,
    }) = accept_request(&mut stream, port).await?
    else {
        return Ok(());
    };
    let cors = allow_origin.as_deref();

    if request.method == "OPTIONS" {
        return respond(
            &mut stream,
            "204 No Content",
            "text/plain; charset=utf-8",
            "",
            cors,
        )
        .await;
    }

    match request.path.as_str() {
        "/mcp" => match request.method.as_str() {
            "POST" => respond_mcp(&mut stream, &registries, &request.body, cors).await,
            _ => {
                respond(
                    &mut stream,
                    "405 Method Not Allowed",
                    "text/plain; charset=utf-8",
                    "MCP endpoint accepts POST requests",
                    cors,
                )
                .await
            }
        },
        "/" | "/index.html" => {
            respond(
                &mut stream,
                "200 OK",
                "text/html; charset=utf-8",
                WEB_HTML,
                cors,
            )
            .await
        }
        "/cytoscape.bundle.js" => {
            respond(
                &mut stream,
                "200 OK",
                "application/javascript; charset=utf-8",
                WEB_CYTOSCAPE,
                cors,
            )
            .await
        }
        "/api/workspaces" => {
            let map = registries.lock().await;
            let body = serde_json::to_string_pretty(&build_workspace_inventory_json(&map))?;
            respond(&mut stream, "200 OK", "application/json", &body, cors).await
        }
        "/api/graph" => match select_registry(&registries, &request.query).await {
            Some(registry) => {
                let body = serde_json::to_string_pretty(&build_graph_json(&registry))?;
                respond(&mut stream, "200 OK", "application/json", &body, cors).await
            }
            None => {
                respond(
                    &mut stream,
                    "200 OK",
                    "application/json",
                    r#"{"crates":[],"nodes":[],"edges":[]}"#,
                    cors,
                )
                .await
            }
        },
        "/api/health" => match select_registry(&registries, &request.query).await {
            Some(registry) => {
                let body = serde_json::to_string_pretty(&build_health_json(&registry))?;
                respond(&mut stream, "200 OK", "application/json", &body, cors).await
            }
            None => respond(&mut stream, "200 OK", "application/json", "{}", cors).await,
        },
        _ => {
            respond(
                &mut stream,
                "404 Not Found",
                "text/plain; charset=utf-8",
                "not found",
                cors,
            )
            .await
        }
    }
}

async fn respond_mcp(
    stream: &mut TcpStream,
    registries: &super::WorkspaceRegistries,
    body: &str,
    allow_origin: Option<&str>,
) -> Result<()> {
    match build_mcp_http_response(registries, body).await? {
        Some(body) => respond(stream, "200 OK", "application/json", &body, allow_origin).await,
        None => {
            respond(
                stream,
                "202 Accepted",
                "text/plain; charset=utf-8",
                "",
                allow_origin,
            )
            .await
        }
    }
}

async fn build_mcp_http_response(
    registries: &super::WorkspaceRegistries,
    body: &str,
) -> Result<Option<String>> {
    let value: Value = match serde_json::from_str(body.trim()) {
        Ok(value) => value,
        Err(error) => {
            let response = JsonRpcResponse::error(None, -32700, format!("Parse error: {error}"));
            return Ok(Some(serde_json::to_string(&response)?));
        }
    };

    if let Value::Array(items) = value {
        if items.is_empty() {
            let response = JsonRpcResponse::error(None, -32600, "Invalid Request: empty batch");
            return Ok(Some(serde_json::to_string(&response)?));
        }

        let mut responses = Vec::new();
        for item in items {
            match serde_json::from_value::<JsonRpcRequest>(item) {
                Ok(request) => {
                    let has_id = request.id.is_some();
                    let response =
                        super::daemon::handle_daemon_request(registries, None, &request).await;
                    if has_id {
                        responses.push(serde_json::to_value(response)?);
                    }
                }
                Err(error) => {
                    responses.push(serde_json::to_value(JsonRpcResponse::error(
                        None,
                        -32600,
                        format!("Invalid Request: {error}"),
                    ))?);
                }
            }
        }

        if responses.is_empty() {
            Ok(None)
        } else {
            Ok(Some(serde_json::to_string(&responses)?))
        }
    } else {
        let request: JsonRpcRequest = match serde_json::from_value(value) {
            Ok(request) => request,
            Err(error) => {
                let response =
                    JsonRpcResponse::error(None, -32600, format!("Invalid Request: {error}"));
                return Ok(Some(serde_json::to_string(&response)?));
            }
        };
        let has_id = request.id.is_some();
        let response = super::daemon::handle_daemon_request(registries, None, &request).await;
        if has_id {
            Ok(Some(serde_json::to_string(&response)?))
        } else {
            Ok(None)
        }
    }
}

/// Resolve the registry for a request: the `workspace` query param if it names a
/// registered workspace, else the first one so a single-workspace daemon works
/// without a selector.
async fn select_registry(
    registries: &super::WorkspaceRegistries,
    query: &str,
) -> Option<Arc<CrateRegistry>> {
    let map = registries.lock().await;
    if let Some(registry) =
        query_param(query, "workspace").and_then(|workspace| map.get(&workspace))
    {
        return Some(Arc::clone(registry));
    }
    map.values().next().map(Arc::clone)
}

/// Extract and percent-decode a query-string parameter.
fn query_param(query: &str, key: &str) -> Option<String> {
    query_params(query, key).into_iter().next()
}

fn query_params(query: &str, key: &str) -> Vec<String> {
    query
        .split('&')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            (k == key).then(|| percent_decode(v))
        })
        .collect()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                match (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2])) {
                    (Some(hi), Some(lo)) => {
                        out.push(hi * 16 + lo);
                        i += 3;
                    }
                    _ => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    registry: Arc<CrateRegistry>,
    port: u16,
) -> Result<()> {
    let Some(GuardedRequest {
        request,
        allow_origin,
    }) = accept_request(&mut stream, port).await?
    else {
        return Ok(());
    };
    let cors = allow_origin.as_deref();

    if request.method == "OPTIONS" {
        return respond(
            &mut stream,
            "204 No Content",
            "text/plain; charset=utf-8",
            "",
            cors,
        )
        .await;
    }

    match request.path.as_str() {
        "/" | "/index.html" => {
            respond(
                &mut stream,
                "200 OK",
                "text/html; charset=utf-8",
                WEB_HTML,
                cors,
            )
            .await
        }
        "/cytoscape.bundle.js" => {
            respond(
                &mut stream,
                "200 OK",
                "application/javascript; charset=utf-8",
                WEB_CYTOSCAPE,
                cors,
            )
            .await
        }
        "/api/graph" => {
            let body = serde_json::to_string_pretty(&build_graph_json(&registry))?;
            respond(&mut stream, "200 OK", "application/json", &body, cors).await
        }
        "/api/health" => {
            let body = serde_json::to_string_pretty(&build_health_json(&registry))?;
            respond(&mut stream, "200 OK", "application/json", &body, cors).await
        }
        _ => {
            respond(
                &mut stream,
                "404 Not Found",
                "text/plain; charset=utf-8",
                "not found",
                cors,
            )
            .await
        }
    }
}

struct HttpRequest {
    method: String,
    path: String,
    query: String,
    body: String,
    origin: Option<String>,
    host: Option<String>,
}

/// Loopback host names the daemon will answer for.
///
/// Anything else in a `Host` header means the request was routed to us through a
/// name we do not own (DNS rebinding), so it is refused.
const LOOPBACK_HOSTS: &[&str] = &["127.0.0.1", "localhost", "[::1]", "::1"];

/// Decide whether a request may be served, and which `Origin` (if any) to echo.
///
/// * `Host` must name a loopback address on our own port. This blocks DNS
///   rebinding, where a public name resolves to 127.0.0.1 to reach us.
/// * `Origin` is only present on browser-issued requests. When present it must be
///   this exact server's origin, so a page on any other site — including
///   `file://`, which sends `Origin: null` — cannot read our responses.
/// * A missing `Origin` is a non-browser client (MCP host, curl); allowed, and
///   nothing is echoed.
fn authorize(request: &HttpRequest, port: u16) -> std::result::Result<Option<String>, String> {
    if let Some(host) = &request.host
        && !host_is_local(host, port)
    {
        return Err(format!("refusing request for non-loopback Host '{host}'"));
    }

    match &request.origin {
        None => Ok(None),
        Some(origin) if origin_is_local(origin, port) => Ok(Some(origin.clone())),
        Some(origin) => Err(format!("refusing cross-origin request from '{origin}'")),
    }
}

fn host_is_local(host: &str, port: u16) -> bool {
    let host = host.trim();
    let (name, host_port) = split_host_port(host);
    match host_port {
        Some(host_port) => host_port == port && LOOPBACK_HOSTS.contains(&name),
        // No port means the default 80; only correct if that is what we bound.
        None => port == 80 && LOOPBACK_HOSTS.contains(&name),
    }
}

fn origin_is_local(origin: &str, port: u16) -> bool {
    let Some(authority) = origin.trim().strip_prefix("http://") else {
        // Includes `null` (file:// / sandboxed frames) and any https origin,
        // neither of which this server can legitimately have served.
        return false;
    };
    let (name, origin_port) = split_host_port(authority);
    origin_port == Some(port) && LOOPBACK_HOSTS.contains(&name)
}

/// Split an authority into host and port, keeping bracketed IPv6 literals intact.
fn split_host_port(authority: &str) -> (&str, Option<u16>) {
    // IPv6 literals are bracketed (`[::1]:8888`), so the port separator is the
    // first `:` after the closing bracket, not the last `:` in the string.
    if let Some(rest) = authority.strip_prefix('[') {
        let Some((name, tail)) = rest.split_once(']') else {
            return (authority, None);
        };
        return (name, tail.strip_prefix(':').and_then(|p| p.parse().ok()));
    }
    match authority.rsplit_once(':') {
        Some((name, port)) => match port.parse() {
            Ok(port) => (name, Some(port)),
            Err(_) => (authority, None),
        },
        None => (authority, None),
    }
}

async fn read_http_request(stream: &mut TcpStream) -> Result<Option<HttpRequest>> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 8192];
    let header_end = loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            if buffer.is_empty() {
                return Ok(None);
            }
            bail!("HTTP request ended before headers were complete");
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(header_end) = find_header_end(&buffer) {
            break header_end;
        }
        if buffer.len() > 1024 * 1024 {
            bail!("HTTP request headers exceeded 1 MiB");
        }
    };

    let headers = String::from_utf8(buffer[..header_end].to_vec())
        .context("HTTP headers were not valid UTF-8")?;
    let mut lines = headers.lines();
    let request_line = lines.next().unwrap_or("GET / HTTP/1.1");
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().unwrap_or("GET").to_string();
    let target = request_parts.next().unwrap_or("/");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));

    let header_lines: Vec<&str> = lines.collect();
    // Conflicting Content-Length headers are a request-smuggling primitive; the
    // only safe response is to refuse rather than pick one.
    let lengths: Vec<usize> = header_lines
        .iter()
        .filter_map(|line| content_length_header(line))
        .collect();
    if lengths.windows(2).any(|pair| pair[0] != pair[1]) {
        bail!("HTTP request carried conflicting Content-Length headers");
    }
    let content_length = lengths.first().copied().unwrap_or_default();
    let origin = header_value(&header_lines, "origin");
    let host = header_value(&header_lines, "host");

    while buffer.len() < header_end + content_length {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            bail!("HTTP request body ended before Content-Length was satisfied");
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > header_end + 16 * 1024 * 1024 {
            bail!("HTTP request body exceeded 16 MiB");
        }
    }

    let body = String::from_utf8(buffer[header_end..header_end + content_length].to_vec())
        .context("HTTP request body was not valid UTF-8")?;

    Ok(Some(HttpRequest {
        method,
        path: path.to_string(),
        query: query.to_string(),
        body,
        origin,
        host,
    }))
}

fn header_value(lines: &[&str], name: &str) -> Option<String> {
    lines.iter().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_string())
    })
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4)
        .or_else(|| {
            buffer
                .windows(2)
                .position(|window| window == b"\n\n")
                .map(|position| position + 2)
        })
}

fn content_length_header(line: &str) -> Option<usize> {
    let (name, value) = line.split_once(':')?;
    name.eq_ignore_ascii_case("content-length")
        .then(|| value.trim().parse().ok())
        .flatten()
}

/// Build the CORS header block for a response.
///
/// Only ever called with an origin `authorize` already accepted, so echoing it
/// cannot widen access. When no origin was allowed the block is empty — notably
/// never `*`, which would hand every site on the internet read access to a
/// loopback service.
fn cors_headers(allow_origin: Option<&str>) -> String {
    match allow_origin {
        Some(origin) => format!(
            "Access-Control-Allow-Origin: {origin}\r\nVary: Origin\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type, MCP-Protocol-Version, Mcp-Session-Id\r\nAccess-Control-Expose-Headers: MCP-Protocol-Version, Mcp-Session-Id\r\n"
        ),
        None => String::new(),
    }
}

async fn respond(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
    allow_origin: Option<&str>,
) -> Result<()> {
    let cors = cors_headers(allow_origin);
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n{cors}Connection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

pub fn build_graph_json(registry: &CrateRegistry) -> Value {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut crates = Vec::new();
    let mut totals = GraphTotals::default();

    let workspace_path = registry.workspace_root().to_string_lossy().to_string();
    let workspace_label = registry
        .workspace_root()
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| workspace_path.clone());
    let workspace_id = node_id(["workspace", &workspace_path]);
    nodes.push(json!({
        "id": workspace_id,
        "label": workspace_label,
        "kind": "workspace",
        "path": workspace_path,
    }));
    totals.workspaces = 1;

    for entry in registry.crates() {
        let crate_key = entry.crate_key();
        let crate_id = node_id(["crate", &crate_key]);
        nodes.push(json!({
            "id": crate_id,
            "label": entry.name,
            "kind": "crate",
            "key": crate_key,
            "path": entry.root.to_string_lossy().to_string(),
        }));
        edges.push(edge(&workspace_id, &crate_id, "contains"));
        totals.crates += 1;

        let model = entry.store.load_actual(&crate_key).ok().flatten();
        let mut crate_stats = GraphTotals::default();
        if let Some(model) = model.as_ref() {
            add_model_graph(
                &crate_key,
                &entry.name,
                &crate_id,
                model,
                &mut nodes,
                &mut edges,
                &mut crate_stats,
            );
            totals.add(&crate_stats);
        }

        crates.push(json!({
            "key": crate_key,
            "name": entry.name,
            "workspace": crate_key,
            "root": entry.root.to_string_lossy().to_string(),
            "has_model": model.is_some(),
            "stats": crate_stats,
        }));
    }

    json!({
        "view": {
            "name": "rust_architecture_overview",
            "visible_node_kinds": ["workspace", "crate", "module", "struct"],
            "visible_edge_kinds": ["contains", "declares", "imports", "calls"],
            "complete_facts_stored": true,
            "hidden_fact_kinds": ["source_file", "enum", "trait", "function", "method", "import_edge", "calls_symbol", "ast_edge"]
        },
        "workspace_root": registry.workspace_root().to_string_lossy(),
        "crates": crates,
        "nodes": nodes,
        "edges": edges,
        "stats": totals,
    })
}

#[cfg(test)]
fn build_graph_json_for_crates(entries: &[GraphCrate]) -> Value {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut crates = Vec::new();
    let mut totals = GraphTotals::default();

    for entry in entries {
        let crate_id = node_id(["crate", &entry.key]);
        nodes.push(json!({
            "id": crate_id,
            "label": entry.name,
            "kind": "crate",
            "key": entry.key,
            "path": entry.root,
        }));
        totals.crates += 1;

        let model = entry.store.load_actual(&entry.key).ok().flatten();
        let mut crate_stats = GraphTotals::default();
        if let Some(model) = model.as_ref() {
            add_model_graph(
                &entry.key,
                &entry.name,
                &crate_id,
                model,
                &mut nodes,
                &mut edges,
                &mut crate_stats,
            );
            totals.add(&crate_stats);
        }

        crates.push(json!({
            "key": entry.key,
            "name": entry.name,
            "root": entry.root,
            "has_model": model.is_some(),
            "stats": crate_stats,
        }));
    }

    json!({
        "view": {
            "name": "rust_architecture_overview",
            "visible_node_kinds": ["crate", "module", "struct"],
            "visible_edge_kinds": ["contains", "declares", "imports", "calls"],
            "complete_facts_stored": true,
            "hidden_fact_kinds": ["source_file", "enum", "trait", "function", "method", "import_edge", "calls_symbol", "ast_edge"]
        },
        "crates": crates,
        "nodes": nodes,
        "edges": edges,
        "stats": totals,
    })
}

fn build_health_json(registry: &CrateRegistry) -> Value {
    let crates: Vec<Value> = registry
        .crates()
        .iter()
        .map(|entry| {
            let crate_key = entry.crate_key();
            let health = entry.store.model_health(&crate_key).ok();
            json!({
                "crate": entry.name,
                "workspace": crate_key,
                "health": health,
            })
        })
        .collect();

    json!({
        "workspace_root": registry.workspace_root().to_string_lossy(),
        "crates": crates,
    })
}

#[cfg(test)]
#[derive(Clone)]
struct GraphCrate {
    key: String,
    name: String,
    root: String,
    store: Arc<crate::store::Store>,
}

#[cfg(test)]
fn graph_crate_from_entry(entry: &crate::store::CrateEntry) -> GraphCrate {
    GraphCrate {
        key: entry.crate_key(),
        name: entry.name.clone(),
        root: entry.root.to_string_lossy().to_string(),
        store: Arc::clone(&entry.store),
    }
}

fn build_workspace_inventory_json(registries: &HashMap<String, Arc<CrateRegistry>>) -> Value {
    let mut items: Vec<Value> = registries
        .iter()
        .map(|(key, registry)| {
            json!({
                "workspace": key,
                "name": std::path::Path::new(key)
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| key.clone()),
                "crates": registry.crates().len(),
            })
        })
        .collect();
    items.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    json!({ "workspaces": items })
}

fn add_model_graph(
    crate_key: &str,
    crate_name: &str,
    crate_id: &str,
    model: &DomainModel,
    nodes: &mut Vec<Value>,
    edges: &mut Vec<Value>,
    totals: &mut GraphTotals,
) {
    let modules = collect_rust_modules(model);
    let module_ids: BTreeMap<String, String> = modules
        .keys()
        .map(|path| (path.clone(), node_id(["module", crate_key, path])))
        .collect();
    let semantic_labels = collect_semantic_labels(model);
    let mut struct_ids_by_name: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut method_owner_ids: BTreeMap<String, String> = BTreeMap::new();
    let mut method_counts_by_owner: BTreeMap<String, usize> = BTreeMap::new();
    let mut architecture_edges: BTreeMap<(String, String, String), usize> = BTreeMap::new();

    for module in modules.values() {
        let id = module_ids
            .get(&module.path)
            .cloned()
            .unwrap_or_else(|| node_id(["module", crate_key, &module.path]));
        nodes.push(json!({
            "id": id,
            "label": module.name,
            "kind": "module",
            "crate": crate_name,
            "crate_key": crate_key,
            "path": module.path,
            "file_path": module.file_path,
            "public": module.public,
            "explicit": module.explicit,
            "file_count": module.file_count,
        }));

        if let Some(parent_path) = &module.parent_path {
            if let Some(parent_id) = module_ids.get(parent_path) {
                edges.push(edge(parent_id, &id, "contains"));
            } else {
                edges.push(edge(crate_id, &id, "contains"));
            }
            totals.submodules += 1;
        } else {
            edges.push(edge(crate_id, &id, "contains"));
        }
        totals.modules += 1;

        let pattern_labels = name_patterns(&module.name.to_ascii_lowercase());
        totals.patterns += pattern_labels.len();
    }

    totals.source_files += model.source_files.len();

    for symbol in &model.symbols {
        totals.symbols += 1;
        match symbol.kind.as_str() {
            "struct" => totals.structs += 1,
            "enum" => totals.enums += 1,
            "trait" => totals.traits += 1,
            "method" => {
                totals.methods += 1;
                if let Some((owner, _)) = symbol.name.split_once("::") {
                    *method_counts_by_owner.entry(owner.to_string()).or_default() += 1;
                }
            }
            "function" => totals.functions += 1,
            _ => {}
        }
    }

    for symbol in model
        .symbols
        .iter()
        .filter(|symbol| symbol.kind == "struct")
    {
        let id = node_id(["struct", crate_key, &symbol.file_path, &symbol.name]);
        struct_ids_by_name
            .entry(symbol.name.clone())
            .or_default()
            .push(id.clone());

        let labels = semantic_labels
            .get(&semantic_label_key(&symbol.context, &symbol.name))
            .cloned()
            .unwrap_or_default();
        totals.semantic_labels += labels.len();

        let pattern_labels = name_patterns(&symbol.name.to_ascii_lowercase());
        totals.patterns += pattern_labels.len();

        nodes.push(json!({
            "id": id,
            "label": symbol.name,
            "kind": "struct",
            "crate": crate_name,
            "crate_key": crate_key,
            "file_path": symbol.file_path,
            "start_line": symbol.start_line,
            "end_line": symbol.end_line,
            "visibility": symbol.visibility,
            "method_count": method_counts_by_owner.get(&symbol.name).copied().unwrap_or_default(),
            "semantic_labels": labels,
            "pattern_labels": pattern_labels,
        }));

        if let Some(module_id) = rust_module_id_for_file(&module_ids, &symbol.file_path) {
            edges.push(edge(&module_id, &id, "declares"));
        } else {
            edges.push(edge(crate_id, &id, "declares"));
        }
    }

    for symbol in model
        .symbols
        .iter()
        .filter(|symbol| symbol.kind == "method")
    {
        let Some((owner, _)) = symbol.name.split_once("::") else {
            continue;
        };
        if let Some(owner_id) = struct_ids_by_name
            .get(owner)
            .and_then(|ids| ids.first())
            .cloned()
        {
            method_owner_ids.insert(symbol.name.clone(), owner_id);
        }
    }

    for import in &model.import_edges {
        totals.imports += 1;
        let Some(from_module_id) = rust_module_id_for_file(&module_ids, &import.from_file) else {
            continue;
        };
        if let Some(to_module_id) = rust_module_id_for_import(&module_ids, &import.to_module)
            && from_module_id != to_module_id
        {
            add_counted_edge(
                &mut architecture_edges,
                &from_module_id,
                &to_module_id,
                "imports",
            );
        }
    }

    for call in &model.call_edges {
        totals.calls += 1;
        let Some(caller_id) =
            struct_id_for_call(&struct_ids_by_name, &method_owner_ids, &call.caller)
        else {
            continue;
        };
        let Some(callee_id) =
            struct_id_for_call(&struct_ids_by_name, &method_owner_ids, &call.callee)
        else {
            continue;
        };
        if caller_id != callee_id {
            add_counted_edge(&mut architecture_edges, &caller_id, &callee_id, "calls");
        }
    }

    flush_counted_edges(architecture_edges, edges);
}

#[derive(Clone)]
struct SemanticLabel {
    label: &'static str,
    confidence: &'static str,
    evidence: String,
}

fn collect_semantic_labels(model: &DomainModel) -> BTreeMap<String, Vec<Value>> {
    let mut labels: BTreeMap<String, Vec<Value>> = BTreeMap::new();

    for context in &model.bounded_contexts {
        for entity in &context.entities {
            push_semantic_label(
                &mut labels,
                &context.name,
                &entity.name,
                SemanticLabel {
                    label: "entity_candidate",
                    confidence: "legacy_heuristic",
                    evidence: "classified by previous DDD overlay".into(),
                },
            );
        }
        for value_object in &context.value_objects {
            push_semantic_label(
                &mut labels,
                &context.name,
                &value_object.name,
                SemanticLabel {
                    label: "value_object_candidate",
                    confidence: "legacy_heuristic",
                    evidence: "classified by previous DDD overlay".into(),
                },
            );
        }
        for service in &context.services {
            push_semantic_label(
                &mut labels,
                &context.name,
                &service.name,
                SemanticLabel {
                    label: "service_candidate",
                    confidence: "legacy_heuristic",
                    evidence: "classified by previous DDD overlay".into(),
                },
            );
        }
        for repository in &context.repositories {
            push_semantic_label(
                &mut labels,
                &context.name,
                &repository.name,
                SemanticLabel {
                    label: "repository_candidate",
                    confidence: "legacy_heuristic",
                    evidence: "classified by previous DDD overlay".into(),
                },
            );
        }
        for event in &context.events {
            push_semantic_label(
                &mut labels,
                &context.name,
                &event.name,
                SemanticLabel {
                    label: "event_candidate",
                    confidence: "legacy_heuristic",
                    evidence: "classified by previous DDD overlay".into(),
                },
            );
        }
    }

    labels
}

fn push_semantic_label(
    labels: &mut BTreeMap<String, Vec<Value>>,
    context: &str,
    symbol: &str,
    label: SemanticLabel,
) {
    labels
        .entry(semantic_label_key(context, symbol))
        .or_default()
        .push(json!({
            "label": label.label,
            "confidence": label.confidence,
            "evidence": label.evidence,
        }));
}

fn semantic_label_key(context: &str, symbol: &str) -> String {
    format!("{context}\u{1f}{symbol}")
}

fn collect_rust_modules(model: &DomainModel) -> BTreeMap<String, GraphModule> {
    let mut modules = BTreeMap::new();

    for source_file in &model.source_files {
        if let Some(path) = rust_module_path_from_file_path(&source_file.path) {
            upsert_module_path(
                &mut modules,
                &path,
                source_file.path.clone(),
                false,
                false,
                true,
            );
        }
    }

    for context in &model.bounded_contexts {
        for module in &context.modules {
            let path = if module.path.is_empty() {
                module.name.clone()
            } else {
                module.path.clone()
            };
            upsert_module_path(
                &mut modules,
                &path,
                module.file_path.clone(),
                module.public,
                true,
                false,
            );
        }
    }

    modules
}

fn rust_module_path_from_file_path(file_path: &str) -> Option<String> {
    let normalized = file_path.replace('\\', "/");
    let parts: Vec<&str> = normalized
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let start = parts
        .iter()
        .position(|part| *part == "src")
        .map_or(0, |index| index + 1);
    let relative = &parts[start..];
    let file_name = relative.last()?;
    if !file_name.ends_with(".rs") {
        return None;
    }

    let mut segments: Vec<String> = relative[..relative.len().saturating_sub(1)]
        .iter()
        .map(|segment| segment.to_string())
        .collect();
    let stem = file_name.trim_end_matches(".rs");
    if stem != "mod" {
        if (stem == "lib" || stem == "main") && segments.is_empty() {
            return None;
        }
        segments.push(stem.to_string());
    }
    (!segments.is_empty()).then(|| segments.join("::"))
}

fn rust_module_id_for_file(
    module_ids: &BTreeMap<String, String>,
    file_path: &str,
) -> Option<String> {
    let mut path = rust_module_path_from_file_path(file_path)?;
    loop {
        if let Some(id) = module_ids.get(&path) {
            return Some(id.clone());
        }
        let (parent, _) = path.rsplit_once("::")?;
        path = parent.to_string();
    }
}

fn rust_module_id_for_import(
    module_ids: &BTreeMap<String, String>,
    import_path: &str,
) -> Option<String> {
    let mut candidate = import_path
        .strip_prefix("crate::")
        .or_else(|| import_path.strip_prefix("super::"))
        .unwrap_or(import_path)
        .trim_end_matches("::*")
        .to_string();

    loop {
        if let Some(id) = module_ids.get(&candidate) {
            return Some(id.clone());
        }
        let (parent, _) = candidate.rsplit_once("::")?;
        candidate = parent.to_string();
    }
}

#[derive(Clone)]
struct GraphModule {
    name: String,
    path: String,
    parent_path: Option<String>,
    file_path: String,
    public: bool,
    explicit: bool,
    file_count: usize,
}

fn upsert_module_path(
    modules: &mut BTreeMap<String, GraphModule>,
    path: &str,
    file_path: String,
    public: bool,
    explicit: bool,
    count_file: bool,
) {
    let segments: Vec<&str> = path
        .split("::")
        .filter(|segment| !segment.is_empty())
        .collect();
    for index in 0..segments.len() {
        let current_path = segments[..=index].join("::");
        let parent_path = (index > 0).then(|| segments[..index].join("::"));
        let entry = modules.entry(current_path.clone()).or_insert(GraphModule {
            name: segments[index].to_string(),
            path: current_path.clone(),
            parent_path,
            file_path: String::new(),
            public: false,
            explicit: false,
            file_count: 0,
        });

        if index == segments.len() - 1 {
            if !file_path.is_empty() {
                entry.file_path = file_path.clone();
            }
            entry.public |= public;
            entry.explicit |= explicit;
            if count_file {
                entry.file_count += 1;
            }
        }
    }
}

fn name_patterns(lower_name: &str) -> Vec<&'static str> {
    let mut patterns = Vec::new();
    if lower_name.contains("facade") || lower_name.contains("gateway") {
        patterns.push("facade_candidate");
    }
    if lower_name.contains("actor")
        || lower_name.contains("worker")
        || lower_name.contains("supervisor")
        || lower_name.contains("watcher")
    {
        patterns.push("actor_candidate");
    }
    if lower_name.contains("adapter")
        || lower_name == "mcp"
        || lower_name == "web"
        || lower_name == "stdio"
        || lower_name.contains("protocol")
    {
        patterns.push("adapter_candidate");
    }
    if lower_name.contains("visitor") {
        patterns.push("visitor_candidate");
    }
    if lower_name.contains("factory") {
        patterns.push("factory_candidate");
    }
    if lower_name.contains("builder") {
        patterns.push("builder_candidate");
    }
    if lower_name.contains("strategy") {
        patterns.push("strategy_candidate");
    }
    patterns
}

fn node_id<const N: usize>(parts: [&str; N]) -> String {
    parts
        .iter()
        .map(|part| {
            part.chars()
                .map(|ch| {
                    if ch.is_ascii_alphanumeric() {
                        ch.to_ascii_lowercase()
                    } else {
                        '-'
                    }
                })
                .collect::<String>()
                .trim_matches('-')
                .to_string()
        })
        .collect::<Vec<_>>()
        .join(":")
}

fn edge(from: &str, to: &str, label: &str) -> Value {
    json!({"from": from, "to": to, "label": label})
}

fn counted_edge(from: &str, to: &str, label: &str, count: usize) -> Value {
    json!({"from": from, "to": to, "label": label, "count": count})
}

fn add_counted_edge(
    edges: &mut BTreeMap<(String, String, String), usize>,
    from: &str,
    to: &str,
    label: &str,
) {
    *edges
        .entry((from.to_string(), to.to_string(), label.to_string()))
        .or_default() += 1;
}

fn flush_counted_edges(
    counted_edges: BTreeMap<(String, String, String), usize>,
    edges: &mut Vec<Value>,
) {
    for ((from, to, label), count) in counted_edges {
        edges.push(counted_edge(&from, &to, &label, count));
    }
}

fn struct_id_for_call(
    struct_ids_by_name: &BTreeMap<String, Vec<String>>,
    method_owner_ids: &BTreeMap<String, String>,
    symbol_name: &str,
) -> Option<String> {
    method_owner_ids.get(symbol_name).cloned().or_else(|| {
        symbol_name
            .split_once("::")
            .and_then(|(owner, _)| struct_ids_by_name.get(owner))
            .or_else(|| struct_ids_by_name.get(symbol_name))
            .and_then(|ids| ids.first())
            .cloned()
    })
}

#[derive(Default, serde::Serialize)]
struct GraphTotals {
    workspaces: usize,
    crates: usize,
    contexts: usize,
    context_dependencies: usize,
    modules: usize,
    submodules: usize,
    source_files: usize,
    structs: usize,
    enums: usize,
    traits: usize,
    functions: usize,
    methods: usize,
    symbols: usize,
    imports: usize,
    calls: usize,
    patterns: usize,
    semantic_labels: usize,
    entities: usize,
    value_objects: usize,
    services: usize,
    repositories: usize,
    events: usize,
}

impl GraphTotals {
    fn add(&mut self, other: &GraphTotals) {
        self.workspaces += other.workspaces;
        self.crates += other.crates;
        self.contexts += other.contexts;
        self.context_dependencies += other.context_dependencies;
        self.modules += other.modules;
        self.submodules += other.submodules;
        self.source_files += other.source_files;
        self.structs += other.structs;
        self.enums += other.enums;
        self.traits += other.traits;
        self.functions += other.functions;
        self.methods += other.methods;
        self.symbols += other.symbols;
        self.imports += other.imports;
        self.calls += other.calls;
        self.patterns += other.patterns;
        self.semantic_labels += other.semantic_labels;
        self.entities += other.entities;
        self.value_objects += other.value_objects;
        self.services += other.services;
        self.repositories += other.repositories;
        self.events += other.events;
    }
}

const WEB_HTML: &str = include_str!("web.html");
const WEB_CYTOSCAPE: &str = include_str!("cytoscape.bundle.js");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::model::*;
    use std::collections::HashMap;
    use std::env::temp_dir;
    use std::fs;
    use tokio::sync::Mutex;

    fn request_with(origin: Option<&str>, host: Option<&str>) -> HttpRequest {
        HttpRequest {
            method: "POST".into(),
            path: "/mcp".into(),
            query: String::new(),
            body: String::new(),
            origin: origin.map(str::to_string),
            host: host.map(str::to_string),
        }
    }

    #[test]
    fn authorize_allows_own_loopback_origins() {
        for origin in [
            "http://127.0.0.1:8888",
            "http://localhost:8888",
            "http://[::1]:8888",
        ] {
            let request = request_with(Some(origin), Some("127.0.0.1:8888"));
            assert_eq!(
                authorize(&request, 8888).unwrap(),
                Some(origin.to_string()),
                "{origin} should be allowed and echoed verbatim"
            );
        }
    }

    #[test]
    fn authorize_rejects_foreign_and_spoofed_origins() {
        for origin in [
            "http://evil.example",
            "null",
            "https://127.0.0.1:8888",
            // Right name, wrong port: a different local server, not us.
            "http://127.0.0.1:9999",
            // Suffix/prefix tricks that a naive `contains` check would accept.
            "http://127.0.0.1.evil.example:8888",
            "http://localhost.evil.example:8888",
        ] {
            let request = request_with(Some(origin), Some("127.0.0.1:8888"));
            assert!(
                authorize(&request, 8888).is_err(),
                "{origin} must be rejected"
            );
        }
    }

    #[test]
    fn authorize_allows_absent_origin_without_echoing_cors() {
        // Non-browser clients (MCP hosts, curl) send no Origin.
        let request = request_with(None, Some("127.0.0.1:8888"));
        assert_eq!(authorize(&request, 8888).unwrap(), None);
    }

    #[test]
    fn authorize_rejects_rebound_host() {
        // DNS rebinding: attacker name resolves to 127.0.0.1, so the connection
        // reaches us, but the Host header still carries their domain.
        let request = request_with(None, Some("evil.example:8888"));
        assert!(authorize(&request, 8888).is_err());
    }

    #[test]
    fn cors_headers_never_emit_a_wildcard() {
        let allowed = cors_headers(Some("http://127.0.0.1:8888"));
        assert!(allowed.contains("Access-Control-Allow-Origin: http://127.0.0.1:8888\r\n"));
        assert!(allowed.contains("Vary: Origin\r\n"));
        assert!(!allowed.contains('*'));

        // No allowed origin means no CORS block at all, so a browser blocks the read.
        assert_eq!(cors_headers(None), "");
    }

    #[test]
    fn split_host_port_handles_ipv6_literals() {
        assert_eq!(split_host_port("[::1]:8888"), ("::1", Some(8888)));
        assert_eq!(split_host_port("127.0.0.1:8888"), ("127.0.0.1", Some(8888)));
        assert_eq!(split_host_port("localhost"), ("localhost", None));
    }

    #[test]
    fn graph_json_contains_actual_model_nodes() {
        let root = temp_dir().join(format!("axon_web_test_{}", std::process::id()));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname='web_test'\nversion='0.1.0'\nedition='2024'\n",
        )
        .unwrap();
        let registry = CrateRegistry::open(&root).unwrap();
        let entry = registry.primary();
        let crate_key = entry.crate_key();
        let model = DomainModel {
            name: "WebTest".into(),
            description: String::new(),
            bounded_contexts: vec![BoundedContext {
                name: "Billing".into(),
                description: String::new(),
                module_path: "src/billing".into(),
                ownership: Ownership::default(),
                aggregates: vec![],
                policies: vec![],
                read_models: vec![],
                entities: vec![Entity {
                    name: "Invoice".into(),
                    description: String::new(),
                    aggregate_root: true,
                    fields: vec![],
                    methods: vec![],
                    invariants: vec![],
                    file_path: Some("src/billing/invoice.rs".into()),
                    start_line: Some(1),
                    end_line: Some(12),
                }],
                value_objects: vec![],
                services: vec![],
                repositories: vec![],
                events: vec![],
                modules: vec![Module {
                    name: "workflow".into(),
                    path: "billing::workflow".into(),
                    public: true,
                    file_path: "src/billing/mod.rs".into(),
                    description: String::new(),
                }],
                dependencies: vec![],
                api_endpoints: vec![],
            }],
            external_systems: vec![],
            architectural_decisions: vec![],
            ownership: Ownership::default(),
            rules: vec![],
            tech_stack: TechStack::default(),
            conventions: Conventions::default(),
            ast_edges: vec![],
            source_files: vec![
                SourceFile {
                    path: "src/billing/mod.rs".into(),
                    context: "Billing".into(),
                    language: "rust".into(),
                },
                SourceFile {
                    path: "src/billing/worker.rs".into(),
                    context: "Billing".into(),
                    language: "rust".into(),
                },
            ],
            symbols: vec![SymbolDef {
                name: "Invoice".into(),
                kind: "struct".into(),
                context: "Billing".into(),
                file_path: "src/billing/invoice.rs".into(),
                start_line: 1,
                end_line: 12,
                visibility: "public".into(),
            }],
            import_edges: vec![ImportEdge {
                from_file: "src/billing/worker.rs".into(),
                to_module: "tokio::sync::mpsc".into(),
                context: "Billing".into(),
            }],
            call_edges: vec![],
            reference_edges: vec![],
        };
        entry.store.save_actual(&crate_key, &model).unwrap();

        let graph = build_graph_json(&registry);
        assert_eq!(graph["view"]["name"], "rust_architecture_overview");
        assert_eq!(graph["view"]["complete_facts_stored"], true);
        assert_eq!(graph["stats"]["workspaces"], 1);
        assert_eq!(graph["stats"]["crates"], 1);
        assert_eq!(
            graph["workspace_root"],
            registry.workspace_root().to_string_lossy().as_ref()
        );
        assert_eq!(graph["stats"]["contexts"], 0);
        assert!(graph["stats"]["modules"].as_u64().unwrap() >= 2);
        assert!(graph["stats"]["submodules"].as_u64().unwrap() >= 1);
        assert_eq!(graph["stats"]["source_files"], 2);
        assert_eq!(graph["stats"]["symbols"], 1);
        assert_eq!(graph["stats"]["structs"], 1);
        assert_eq!(graph["stats"]["semantic_labels"], 1);
        assert!(graph["nodes"].as_array().unwrap().iter().all(|node| {
            matches!(
                node["kind"].as_str(),
                Some("workspace" | "crate" | "module" | "struct")
            )
        }));
        assert!(graph["nodes"].as_array().unwrap().iter().any(|node| {
            node["kind"] == "struct"
                && node["label"] == "Invoice"
                && node["semantic_labels"][0]["label"] == "entity_candidate"
        }));
        assert!(
            graph["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|node| { node["kind"] == "module" && node["path"] == "billing::workflow" })
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn graph_json_does_not_duplicate_root_modules_for_lib_and_main() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = temp_dir().join(format!("axon_web_lib_main_test_{unique}"));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname='lib_main_web_test'\nversion='0.1.0'\nedition='2024'\n",
        )
        .unwrap();
        fs::write(root.join("src/lib.rs"), "pub mod app;\n").unwrap();
        fs::write(root.join("src/main.rs"), "mod app;\nfn main() {}\n").unwrap();
        fs::write(root.join("src/app.rs"), "pub struct App;\n").unwrap();

        let registry = CrateRegistry::open(&root).unwrap();
        let entry = registry.primary();
        let crate_key = entry.crate_key();
        let actual = crate::domain::analyze::scan_actual_model(&root, None).unwrap();
        entry.store.save_actual(&crate_key, &actual).unwrap();

        let graph = build_graph_json(&registry);
        let module_paths = graph["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|node| node["kind"] == "module")
            .filter_map(|node| node["path"].as_str())
            .collect::<Vec<_>>();

        assert!(module_paths.contains(&"app"));
        assert!(
            !module_paths
                .iter()
                .any(|path| *path != "app" && path.ends_with("::app")),
            "root module should appear once as app, got {module_paths:?}"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn graph_json_uses_crate_keys_for_duplicate_crate_names() {
        use std::collections::BTreeSet;

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let parent_a = temp_dir().join(format!("axon_web_dupe_a_{unique}"));
        let parent_b = temp_dir().join(format!("axon_web_dupe_b_{unique}"));
        let root_a = parent_a.join("shared");
        let root_b = parent_b.join("shared");
        for root in [&root_a, &root_b] {
            fs::create_dir_all(root.join("src")).unwrap();
            fs::write(
                root.join("Cargo.toml"),
                "[package]\nname='shared'\nversion='0.1.0'\nedition='2024'\n",
            )
            .unwrap();
            fs::write(root.join("src/lib.rs"), "pub struct App;\n").unwrap();
        }

        let registry_a = CrateRegistry::open(&root_a).unwrap();
        let registry_b = CrateRegistry::open(&root_b).unwrap();
        let entries = vec![
            graph_crate_from_entry(registry_a.primary()),
            graph_crate_from_entry(registry_b.primary()),
        ];

        let graph = build_graph_json_for_crates(&entries);
        let crate_ids: BTreeSet<&str> = graph["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|node| node["kind"] == "crate")
            .filter_map(|node| node["id"].as_str())
            .collect();

        assert_eq!(graph["stats"]["crates"], 2);
        assert_eq!(graph["stats"]["workspaces"], 0);
        assert_eq!(crate_ids.len(), 2);
        assert_eq!(graph["crates"].as_array().unwrap().len(), 2);

        let _ = fs::remove_dir_all(parent_a);
        let _ = fs::remove_dir_all(parent_b);
    }

    #[test]
    fn web_page_contains_graph_bootstrap() {
        assert!(WEB_HTML.contains("/api/graph"));
        assert!(WEB_HTML.contains("/api/workspaces"));
        assert!(WEB_HTML.contains("Live Rust architecture overview"));
        assert!(WEB_HTML.contains("cytoscape"));
        assert!(WEB_HTML.contains("/cytoscape.bundle.js"));
        assert!(WEB_HTML.contains("id=\"workspace-field\""));
        assert!(WEB_HTML.contains("id=\"layout-mode\""));
        assert!(WEB_HTML.contains("grid-template-columns: 1fr;"));
        assert!(!WEB_HTML.contains("id=\"crate-toggle\""));
        assert!(!WEB_HTML.contains("/api/crates"));
        assert!(!WEB_HTML.contains("<h2>Legend</h2>"));
        assert!(!WEB_HTML.contains("class=\"legend\""));
        assert!(!WEB_HTML.contains("Source file"));
        assert!(WEB_CYTOSCAPE.contains("cytoscape"));
    }

    #[tokio::test]
    async fn mcp_http_tools_list_is_global() {
        let registries = Arc::new(Mutex::new(HashMap::new()));
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#;

        let response = build_mcp_http_response(&registries, body)
            .await
            .unwrap()
            .expect("tools/list should return a response");
        let response: Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 1);
        assert!(
            response["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| {
                    tool["name"] == "rust_status"
                        && tool["inputSchema"]["properties"]
                            .get("workspace_path")
                            .is_some()
                })
        );
    }

    #[tokio::test]
    async fn mcp_http_tool_call_requires_workspace_context() {
        let registries = Arc::new(Mutex::new(HashMap::new()));
        let body = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"rust_status","arguments":{"detail":"summary"}}}"#;

        let response = build_mcp_http_response(&registries, body)
            .await
            .unwrap()
            .expect("tool errors should return a JSON-RPC response");
        let response: Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 2);
        assert_eq!(response["error"]["code"], -32602);
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap()
                .contains("workspace_path")
        );
    }

    #[tokio::test]
    async fn mcp_http_notification_returns_no_body() {
        let registries = Arc::new(Mutex::new(HashMap::new()));
        let body = r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#;

        let response = build_mcp_http_response(&registries, body).await.unwrap();

        assert!(response.is_none());
    }
}
