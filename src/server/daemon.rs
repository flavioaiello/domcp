//! Long-running, in-memory, multi-workspace daemon.
//!
//! One process holds a `workspace-root → CrateRegistry` map entirely in memory,
//! so every editor session (each a thin [`super::bridge`]) shares one warm brain
//! and every workspace is isolated by its canonical root path. Workspaces are
//! registered lazily on first use, then kept fresh by a per-workspace watcher.
//!
//! Transport is a Unix domain socket speaking the same newline-delimited
//! JSON-RPC as the stdio server; each connection begins with a one-line
//! handshake. The handshake may include a legacy default `workspace`, but normal
//! daemon routing uses workspace context supplied on each tool call.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;
use tracing::{info, warn};

use super::WorkspaceRegistries as Registries;
use super::watcher::ActualStateWatcher;
use crate::mcp::protocol::{JsonRpcRequest, JsonRpcResponse};
use crate::mcp::{handle_global_request, handle_request_with_registry, parse_tool_call_params};
use crate::store::{CrateRegistry, canonicalize_path};

#[derive(Deserialize)]
struct Handshake {
    #[serde(default)]
    workspace: Option<String>,
}

/// Run the daemon, listening on `socket_path` until the process is stopped.
/// `web_port` hosts the multi-workspace web graph.
pub async fn run(socket_path: &Path, web_port: u16) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create socket directory {}", parent.display()))?;
    }
    // Refuse to start a second daemon over a live one: a single shared brain
    // owns the socket. (Removing the socket below would otherwise orphan it.)
    if tokio::net::UnixStream::connect(socket_path).await.is_ok() {
        anyhow::bail!(
            "an axon daemon is already listening on {}",
            socket_path.display()
        );
    }
    // A stale socket file from a previous run would block bind().
    let _ = std::fs::remove_file(socket_path);

    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("bind daemon socket {}", socket_path.display()))?;
    info!("Axon daemon listening on {}", socket_path.display());

    let registries: Registries = Arc::new(Mutex::new(HashMap::new()));

    // Best-effort multi-workspace web graph on the default port. The bridges
    // never open it, so the daemon owns the single port (no collisions).
    {
        let registries = Arc::clone(&registries);
        tokio::spawn(async move {
            if let Err(e) = super::web::run_multi(registries, web_port).await {
                warn!("daemon web graph unavailable: {e:#}");
            }
        });
    }

    loop {
        let (stream, _) = listener.accept().await?;
        let registries = Arc::clone(&registries);
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, registries).await {
                warn!("daemon connection error: {e:#}");
            }
        });
    }
}

async fn handle_connection(stream: UnixStream, registries: Registries) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();

    // First line is a transport handshake. `workspace` is an optional legacy
    // default; normal routing is resolved from each tool call's arguments.
    let Some(first) = lines.next_line().await? else {
        return Ok(());
    };
    let handshake: Handshake = serde_json::from_str(first.trim())
        .context("daemon: first line must be a JSON handshake object")?;
    let default_workspace = handshake
        .workspace
        .filter(|workspace| !workspace.is_empty());

    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let request: JsonRpcRequest = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                let resp = JsonRpcResponse::error(None, -32700, format!("Parse error: {e}"));
                send_line(&mut write_half, &resp).await?;
                continue;
            }
        };
        // CozoDB work is synchronous but fast; the heavy save happens off the
        // request path in the watcher. Notifications (no id) get no response.
        let response =
            handle_daemon_request(&registries, default_workspace.as_deref(), &request).await;
        if request.id.is_some() {
            send_line(&mut write_half, &response).await?;
        }
    }
    Ok(())
}

pub(super) async fn handle_daemon_request(
    registries: &Registries,
    default_workspace: Option<&str>,
    request: &JsonRpcRequest,
) -> JsonRpcResponse {
    if let Some(response) = handle_global_request(request) {
        return response;
    }

    let workspace = if request.method == "tools/call" {
        let params = match parse_tool_call_params(request) {
            Ok(params) => params,
            Err(response) => return *response,
        };
        match workspace_for_tool_call(&params.arguments, default_workspace) {
            Ok(workspace) => workspace,
            Err(message) => return JsonRpcResponse::error(request.id.clone(), -32602, message),
        }
    } else if let Some(workspace) = default_workspace {
        canonicalize_path(workspace)
    } else {
        return JsonRpcResponse::error(
            request.id.clone(),
            -32602,
            format!(
                "Workspace context required for {}. Pass workspace_path or file_path in tool arguments.",
                request.method
            ),
        );
    };

    let registry = match ensure_registry(registries, &workspace).await {
        Ok(registry) => registry,
        Err(error) => {
            return JsonRpcResponse::error(
                request.id.clone(),
                -32000,
                format!(
                    "Workspace registration failed for '{}': {error:#}. Pass a valid workspace_path or file_path with the tool call.",
                    workspace
                ),
            );
        }
    };
    // Tool dispatch is fully synchronous and can be very slow: `rust_scan` runs a
    // whole rust-analyzer workspace load, which shells out to cargo. Running that
    // inline would pin a runtime worker for the duration and stall every other
    // client sharing this daemon, so it goes to the blocking pool.
    let request = request.clone();
    let request_id = request.id.clone();
    match tokio::task::spawn_blocking(move || handle_request_with_registry(&registry, &request))
        .await
    {
        Ok(response) => response,
        Err(error) => JsonRpcResponse::error(
            request_id,
            -32000,
            format!("Tool execution failed: {error}"),
        ),
    }
}

fn workspace_for_tool_call(
    args: &serde_json::Value,
    default_workspace: Option<&str>,
) -> std::result::Result<String, String> {
    for key in ["workspace_path", "workspace"] {
        if let Some(workspace) = string_arg(args, key) {
            return Ok(canonicalize_path(workspace));
        }
    }

    for key in ["file_path", "path"] {
        if let Some(path) = string_arg(args, key) {
            let route_path = route_context_path(path, default_workspace)?;
            if let Some(workspace) = infer_workspace_from_path(&route_path) {
                return Ok(workspace.to_string_lossy().into_owned());
            }
            return Err(format!(
                "Could not infer a Cargo workspace from {}: {}. Pass workspace_path with the tool call.",
                key,
                route_path.display()
            ));
        }
    }

    default_workspace
        .map(canonicalize_path)
        .ok_or_else(|| "Workspace context required for tools/call. Pass workspace_path or file_path in arguments.".to_string())
}

fn string_arg<'a>(args: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
}

fn route_context_path(
    path: &str,
    default_workspace: Option<&str>,
) -> std::result::Result<PathBuf, String> {
    let path = PathBuf::from(path);
    let path = if path.is_absolute() {
        path
    } else if let Some(workspace) = default_workspace {
        Path::new(workspace).join(path)
    } else {
        return Err("Relative file_path/path requires workspace_path when no session default workspace is set.".to_string());
    };
    Ok(path.canonicalize().unwrap_or(path))
}

fn infer_workspace_from_path(start: &Path) -> Option<PathBuf> {
    let start = if start.is_file() || (start.extension().is_some() && !start.is_dir()) {
        start.parent()?
    } else {
        start
    };
    let mut package_root = None;

    for candidate in start.ancestors() {
        if candidate.parent().is_none() {
            break;
        }

        let cargo_toml = candidate.join("Cargo.toml");
        if !cargo_toml.is_file() {
            continue;
        }

        if cargo_toml_declares_workspace(&cargo_toml) {
            return Some(canonicalize_or_self(candidate));
        }

        if package_root.is_none() && candidate.join("src").is_dir() {
            package_root = Some(canonicalize_or_self(candidate));
        }
    }

    package_root
}

fn cargo_toml_declares_workspace(cargo_toml: &Path) -> bool {
    let Ok(contents) = std::fs::read_to_string(cargo_toml) else {
        return false;
    };

    contents.lines().any(|line| {
        let line = line.split('#').next().unwrap_or_default().trim();
        line == "[workspace]" || line.starts_with("[workspace.")
    })
}

fn canonicalize_or_self(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Ceiling on simultaneously registered workspaces.
///
/// Each registration permanently costs a CozoDB store per crate plus a recursive
/// filesystem watcher, and nothing is ever released: the watcher task holds its
/// own `Arc<CrateRegistry>`, so dropping the map entry would neither free the
/// store nor stop the watch. Until watcher shutdown exists, a hard ceiling is the
/// honest bound — it caps the cost instead of pretending to reclaim it.
const MAX_REGISTERED_WORKSPACES: usize = 32;

/// Return the registry for `workspace`, building, scanning, and starting a
/// watcher for it on first use. Keyed by canonical path so symlinked or relative
/// variants resolve to the same in-memory model.
async fn ensure_registry(registries: &Registries, workspace: &str) -> Result<Arc<CrateRegistry>> {
    let key = canonicalize_path(workspace);
    {
        let map = registries.lock().await;
        if let Some(existing) = map.get(&key) {
            return Ok(Arc::clone(existing));
        }
        anyhow::ensure!(
            map.len() < MAX_REGISTERED_WORKSPACES,
            "daemon is already tracking {MAX_REGISTERED_WORKSPACES} workspaces; \
             restart the daemon to register a different one"
        );
    }

    // Build outside the map lock: `CrateRegistry::open` walks the tree and opens a
    // CozoDB per crate, and holding the lock across that would serialize every
    // other workspace's requests behind one cold registration.
    let open_key = key.clone();
    let registry = tokio::task::spawn_blocking(move || {
        CrateRegistry::open(Path::new(&open_key))
            .with_context(|| format!("open workspace {open_key}"))
    })
    .await
    .context("workspace registration task panicked")??;
    let registry = Arc::new(registry);

    // Two callers can race here; the first insert wins so every caller shares one
    // registry (and only one watcher is ever started for it).
    let mut map = registries.lock().await;
    if let Some(existing) = map.get(&key) {
        return Ok(Arc::clone(existing));
    }
    info!(
        "daemon registered workspace {} ({} crate(s))",
        key,
        registry.crates().len()
    );
    map.insert(key.clone(), Arc::clone(&registry));
    drop(map);

    // Keep this workspace's model fresh without blocking MCP startup. The
    // initial scan can take long enough for clients to time out while waiting
    // for initialize/tools/list responses.
    let watcher_registry = Arc::clone(&registry);
    tokio::spawn(async move {
        let watcher = ActualStateWatcher::new(watcher_registry);
        if let Err(error) = watcher.spawn().await {
            warn!("failed to start watcher for workspace {key}: {error:#}");
        }
    });

    Ok(registry)
}

async fn send_line(
    write_half: &mut tokio::net::unix::OwnedWriteHalf,
    resp: &JsonRpcResponse,
) -> Result<()> {
    let mut json = serde_json::to_string(resp)?;
    json.push('\n');
    write_half.write_all(json.as_bytes()).await?;
    write_half.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn daemon_handshake_and_initialize_round_trip() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        // Minimal temp crate to register as a workspace.
        let root = std::env::temp_dir().join(format!("axon_daemon_test_{unique}"));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src").join("lib.rs"),
            "pub struct Foo { pub x: u64 }\n",
        )
        .unwrap();

        // Keep the socket path short: macOS Unix-domain socket paths are capped
        // around 100 bytes, while `std::env::temp_dir()` can be much longer.
        let socket = std::path::PathBuf::from(format!("/tmp/axon_daemon_{unique}.sock"));
        let socket_run = socket.clone();
        // web_port 0 → ephemeral, so tests never collide on a fixed port.
        tokio::spawn(async move {
            if let Err(e) = run(&socket_run, 0).await {
                eprintln!("daemon test server failed: {e:#}");
            }
        });

        // Wait until the listener accepts connections; socket-file visibility can
        // race with bind readiness on CI and slower local runs.
        let mut stream = None;
        for _ in 0..100 {
            match UnixStream::connect(&socket).await {
                Ok(connected) => {
                    stream = Some(connected);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
        let stream = stream.expect("connect daemon");
        let (read_half, mut write_half) = stream.into_split();
        let mut lines = BufReader::new(read_half).lines();

        let handshake = serde_json::json!({}).to_string();
        write_half.write_all(handshake.as_bytes()).await.unwrap();
        write_half.write_all(b"\n").await.unwrap();
        write_half
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n")
            .await
            .unwrap();
        write_half.flush().await.unwrap();

        let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
            .await
            .expect("daemon response timed out")
            .unwrap()
            .expect("daemon closed without responding");
        let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert!(
            resp.get("result").is_some(),
            "initialize should return a result, got: {line}"
        );

        let mut warmed_status = None;
        for id in 2..=21 {
            let request = format!(
                r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"rust_status","arguments":{{"workspace_path":{},"detail":"full"}}}}}}
"#,
                serde_json::to_string(&root.to_string_lossy()).unwrap()
            );
            write_half.write_all(request.as_bytes()).await.unwrap();
            write_half.flush().await.unwrap();

            let line = tokio::time::timeout(Duration::from_secs(1), lines.next_line())
                .await
                .expect("daemon rust_status response timed out")
                .unwrap()
                .expect("daemon closed without responding to rust_status");
            let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(resp["jsonrpc"], "2.0");
            assert_eq!(resp["id"], id);
            let Some(content) = resp["result"]["content"].as_array() else {
                continue;
            };
            let Some(text) = content[0]["text"].as_str() else {
                continue;
            };
            let status: serde_json::Value = serde_json::from_str(text).unwrap();
            let context_count = status
                .pointer("/implemented/bounded_contexts")
                .and_then(|value| value.as_array())
                .map(|contexts| contexts.len())
                .unwrap_or_default();
            if context_count == 1 {
                warmed_status = Some(status);
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        let status = warmed_status.expect("daemon model did not warm up");
        assert_eq!(
            status
                .pointer("/truth_maintenance/drift/entry_count")
                .and_then(|value| value.as_i64()),
            Some(0)
        );

        let _ = std::fs::remove_file(&socket);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn daemon_invalid_workspace_returns_json_rpc_error() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        let root = std::env::temp_dir().join(format!("axon_empty_workspace_{unique}"));
        std::fs::create_dir_all(&root).unwrap();
        let socket = std::path::PathBuf::from(format!("/tmp/axon_daemon_invalid_{unique}.sock"));
        let socket_run = socket.clone();
        tokio::spawn(async move {
            if let Err(e) = run(&socket_run, 0).await {
                eprintln!("daemon invalid-workspace test server failed: {e:#}");
            }
        });

        let mut stream = None;
        for _ in 0..100 {
            match UnixStream::connect(&socket).await {
                Ok(connected) => {
                    stream = Some(connected);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
        let stream = stream.expect("connect daemon");
        let (read_half, mut write_half) = stream.into_split();
        let mut lines = BufReader::new(read_half).lines();

        let handshake = serde_json::json!({}).to_string();
        write_half.write_all(handshake.as_bytes()).await.unwrap();
        write_half.write_all(b"\n").await.unwrap();
        write_half
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n")
            .await
            .unwrap();
        write_half.flush().await.unwrap();

        let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
            .await
            .expect("daemon initialize response timed out")
            .unwrap()
            .expect("daemon closed without initialize response");
        let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert!(
            resp.get("result").is_some(),
            "initialize should be global: {line}"
        );

        let missing_context_request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "rust_status",
                "arguments": {
                    "detail": "full"
                }
            }
        })
        .to_string();
        write_half
            .write_all(missing_context_request.as_bytes())
            .await
            .unwrap();
        write_half.write_all(b"\n").await.unwrap();
        write_half.flush().await.unwrap();

        let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
            .await
            .expect("daemon missing-context response timed out")
            .unwrap()
            .expect("daemon closed without missing-context response");
        let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 2);
        assert_eq!(resp["error"]["code"], -32602);
        assert!(
            resp["error"]["message"]
                .as_str()
                .unwrap()
                .contains("workspace_path"),
            "missing-context error should explain per-call workspace context: {line}"
        );

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "rust_status",
                "arguments": {
                    "workspace_path": root.to_string_lossy(),
                    "detail": "full"
                }
            }
        })
        .to_string();
        write_half.write_all(request.as_bytes()).await.unwrap();
        write_half.write_all(b"\n").await.unwrap();
        write_half.flush().await.unwrap();

        let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
            .await
            .expect("daemon tool error response timed out")
            .unwrap()
            .expect("daemon closed without tool error response");
        let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 3);
        assert_eq!(resp["error"]["code"], -32000);
        assert!(
            resp["error"]["message"]
                .as_str()
                .unwrap()
                .contains("workspace_path"),
            "error should explain per-call workspace context: {line}"
        );

        let _ = std::fs::remove_file(&socket);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn bridge_reports_when_no_daemon() {
        // Connecting to a non-existent socket must signal absence, not error.
        let missing = std::env::temp_dir().join("axon_no_such_daemon.sock");
        let _ = std::fs::remove_file(&missing);
        let bridged = super::super::bridge::try_bridge(&missing, Some("/tmp/whatever"))
            .await
            .expect("try_bridge should not error when daemon is absent");
        assert!(
            !bridged,
            "no daemon -> bridge should return false so caller can retry or abort"
        );
    }
}
