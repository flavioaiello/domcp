use anyhow::Result;
use serde_json::json;
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::domain::model::DomainModel;
use crate::mcp::{protocol::*, prompts, resources, tools, write_tools};
use crate::store::Store;

/// List of write-tool names used to route `tools/call` to the mutable path.
const WRITE_TOOLS: &[&str] = &[
    "update_bounded_context",
    "update_entity",
    "update_service",
    "update_event",
    "remove_entity",
    "compare_model",
    "draft_refactoring_plan",
    "save_model",
];

/// Run the MCP server over stdio (stdin/stdout), the standard transport for
/// VS Code / GitHub Copilot MCP integration.
pub async fn run(mut model: DomainModel, workspace_path: String, store: Store) -> Result<()> {
    let stdin = BufReader::new(io::stdin());
    let mut stdout = io::stdout();
    let mut lines = stdin.lines();

    tracing::info!("DOMCP stdio transport ready");

    while let Some(line) = lines.next_line().await? {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        tracing::debug!("← {}", line);

        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let resp = JsonRpcResponse::error(None, -32700, format!("Parse error: {e}"));
                send(&mut stdout, &resp).await?;
                continue;
            }
        };

        let response = handle_request(&mut model, &workspace_path, &store, &request);

        // Notifications (no id) don't get a response
        if request.id.is_some() {
            send(&mut stdout, &response).await?;
        }
    }

    Ok(())
}

fn handle_request(
    model: &mut DomainModel,
    workspace_path: &str,
    store: &Store,
    req: &JsonRpcRequest,
) -> JsonRpcResponse {
    match req.method.as_str() {
        // ── Lifecycle ──────────────────────────────────────────────
        "initialize" => {
            let result = InitializeResult {
                protocol_version: "2025-03-26".into(),
                capabilities: ServerCapabilities {
                    tools: Some(ToolsCapability {}),
                    resources: Some(ResourcesCapability {}),
                    prompts: Some(PromptsCapability {}),
                },
                server_info: ServerInfo {
                    name: format!("domcp ({})", model.name),
                    version: env!("CARGO_PKG_VERSION").into(),
                },
            };
            JsonRpcResponse::success(req.id.clone(), serde_json::to_value(result).unwrap())
        }

        // notifications — no response needed
        "notifications/initialized" | "initialized" => {
            JsonRpcResponse::success(req.id.clone(), json!({}))
        }

        // ── Tools ──────────────────────────────────────────────────
        "tools/list" => {
            let mut all_tools = tools::list_tools();
            all_tools.extend(write_tools::list_write_tools());
            let result = ToolsListResult { tools: all_tools };
            JsonRpcResponse::success(req.id.clone(), serde_json::to_value(result).unwrap())
        }

        "tools/call" => {
            let params: ToolCallParams = match req.params.as_ref() {
                Some(p) => match serde_json::from_value(p.clone()) {
                    Ok(p) => p,
                    Err(e) => {
                        return JsonRpcResponse::error(
                            req.id.clone(),
                            -32602,
                            format!("Invalid params: {e}"),
                        );
                    }
                },
                None => {
                    return JsonRpcResponse::error(
                        req.id.clone(),
                        -32602,
                        "Missing params",
                    );
                }
            };

            let result = if WRITE_TOOLS.contains(&params.name.as_str()) {
                write_tools::call_write_tool(model, workspace_path, store, &params.name, &params.arguments)
            } else {
                tools::call_tool(model, &params.name, &params.arguments)
            };
            JsonRpcResponse::success(req.id.clone(), serde_json::to_value(result).unwrap())
        }

        // ── Resources ──────────────────────────────────────────────
        "resources/list" => {
            let result = ResourcesListResult {
                resources: resources::list_resources(model),
            };
            JsonRpcResponse::success(req.id.clone(), serde_json::to_value(result).unwrap())
        }

        "resources/read" => {
            let params: ResourceReadParams = match req.params.as_ref() {
                Some(p) => match serde_json::from_value(p.clone()) {
                    Ok(p) => p,
                    Err(e) => {
                        return JsonRpcResponse::error(
                            req.id.clone(),
                            -32602,
                            format!("Invalid params: {e}"),
                        );
                    }
                },
                None => {
                    return JsonRpcResponse::error(
                        req.id.clone(),
                        -32602,
                        "Missing params",
                    );
                }
            };

            let result = resources::read_resource(model, &params.uri);
            JsonRpcResponse::success(req.id.clone(), serde_json::to_value(result).unwrap())
        }

        // ── Prompts ─────────────────────────────────────────────────────
        "prompts/list" => {
            let result = PromptsListResult {
                prompts: prompts::list_prompts(),
            };
            JsonRpcResponse::success(req.id.clone(), serde_json::to_value(result).unwrap())
        }

        "prompts/get" => {
            let params: PromptGetParams = match req.params.as_ref() {
                Some(p) => match serde_json::from_value(p.clone()) {
                    Ok(p) => p,
                    Err(e) => {
                        return JsonRpcResponse::error(
                            req.id.clone(),
                            -32602,
                            format!("Invalid params: {e}"),
                        );
                    }
                },
                None => {
                    return JsonRpcResponse::error(
                        req.id.clone(),
                        -32602,
                        "Missing params",
                    );
                }
            };

            match prompts::get_prompt(model, &params.name) {
                Some(result) => {
                    JsonRpcResponse::success(req.id.clone(), serde_json::to_value(result).unwrap())
                }
                None => JsonRpcResponse::error(
                    req.id.clone(),
                    -32602,
                    format!("Prompt not found: {}", params.name),
                ),
            }
        }

        // ── Ping (required by MCP spec) ────────────────────────────
        "ping" => JsonRpcResponse::success(req.id.clone(), json!({})),

        // ── Unknown ────────────────────────────────────────────────
        method => JsonRpcResponse::error(
            req.id.clone(),
            -32601,
            format!("Method not found: {method}"),
        ),
    }
}

async fn send(stdout: &mut io::Stdout, resp: &JsonRpcResponse) -> Result<()> {
    let json = serde_json::to_string(resp)?;
    tracing::debug!("→ {}", json);
    stdout.write_all(json.as_bytes()).await?;
    stdout.write_all(b"\n").await?;
    stdout.flush().await?;
    Ok(())
}
