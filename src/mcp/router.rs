use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;

use super::{
    load_actual_model, prompts,
    protocol::{
        InitializeResult, JsonRpcRequest, JsonRpcResponse, PromptGetParams, PromptsCapability,
        PromptsListResult, ResourceReadParams, ResourcesCapability, ResourcesListResult,
        ServerCapabilities, ServerInfo, ToolCallParams, ToolsCapability, ToolsListResult,
    },
    resources, tools, write_tools,
};
use crate::store::{CrateEntry, CrateRegistry, Store};

/// List of write-tool names used to route `tools/call` to the mutable path.
const WRITE_TOOLS: &[&str] = &[
    "rust_scan",
    "rust_annotations",
    "rust_diagnose",
    "rust_constraints",
];

pub(crate) fn handle_request_with_registry(
    registry: &CrateRegistry,
    req: &JsonRpcRequest,
) -> JsonRpcResponse {
    if req.method != "tools/call" {
        let primary = registry.primary();
        let workspace_path = primary.workspace_key();
        return handle_request(&workspace_path, &primary.store, req);
    }

    let params: ToolCallParams = match parse_params(req) {
        Ok(params) => params,
        Err(response) => return *response,
    };

    let entry = match select_tool_entry(registry, &params.arguments) {
        Ok(entry) => entry,
        Err(message) => return JsonRpcResponse::error(req.id.clone(), -32602, message),
    };
    let workspace_path = entry.workspace_key();
    let result = if WRITE_TOOLS.contains(&params.name.as_str()) {
        write_tools::call_write_tool(
            &workspace_path,
            &entry.store,
            &params.name,
            &params.arguments,
        )
    } else {
        tools::call_tool(
            &entry.store,
            &workspace_path,
            &params.name,
            &params.arguments,
        )
    };
    success_response(req, result)
}

pub(crate) fn handle_global_request(req: &JsonRpcRequest) -> Option<JsonRpcResponse> {
    match req.method.as_str() {
        "initialize" => {
            let client_version = req
                .params
                .as_ref()
                .and_then(|p| p.get("protocolVersion"))
                .and_then(|v| v.as_str())
                .unwrap_or("2024-11-05");

            let result = InitializeResult {
                protocol_version: client_version.into(),
                capabilities: ServerCapabilities {
                    tools: Some(ToolsCapability {}),
                    resources: Some(ResourcesCapability {}),
                    prompts: Some(PromptsCapability {}),
                },
                server_info: ServerInfo {
                    name: "axon".into(),
                    version: crate::VERSION.into(),
                },
            };
            Some(success_response(req, result))
        }
        "notifications/initialized" | "initialized" => Some(success_response(req, json!({}))),
        "tools/list" => {
            let mut all_tools = tools::list_tools();
            all_tools.extend(write_tools::list_write_tools());
            let result = ToolsListResult { tools: all_tools };
            Some(success_response(req, result))
        }
        "prompts/list" => {
            let result = PromptsListResult {
                prompts: prompts::list_prompts(),
            };
            Some(success_response(req, result))
        }
        "ping" => Some(success_response(req, json!({}))),
        _ => None,
    }
}

pub(crate) fn parse_tool_call_params(
    req: &JsonRpcRequest,
) -> std::result::Result<ToolCallParams, Box<JsonRpcResponse>> {
    parse_params(req)
}

fn select_tool_entry<'a>(
    registry: &'a CrateRegistry,
    args: &serde_json::Value,
) -> std::result::Result<&'a CrateEntry, String> {
    if let Some(crate_name) = args
        .get("crate")
        .or_else(|| args.get("crate_name"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
    {
        return registry
            .by_name(crate_name)
            .ok_or_else(|| format!("Unknown crate: {crate_name}"));
    }

    for key in ["workspace", "workspace_path"] {
        if let Some(path) = args
            .get(key)
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
        {
            let route_path = route_path(registry, path);
            if route_path == registry.workspace_root() {
                return Ok(registry.primary());
            }
            return registry.for_path(&route_path).ok_or_else(|| {
                format!(
                    "No discovered crate owns {} route: {}",
                    key,
                    route_path.display()
                )
            });
        }
    }

    for key in ["path", "file_path"] {
        if let Some(path) = args
            .get(key)
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
        {
            let route_path = route_path(registry, path);
            return registry.for_path(&route_path).ok_or_else(|| {
                format!(
                    "No discovered crate owns {} route: {}",
                    key,
                    route_path.display()
                )
            });
        }
    }

    Ok(registry.primary())
}

fn route_path(registry: &CrateRegistry, path: &str) -> std::path::PathBuf {
    let path = std::path::PathBuf::from(path);
    let path = if path.is_absolute() {
        path
    } else {
        registry.workspace_root().join(path)
    };
    path.canonicalize().unwrap_or(path)
}

fn parse_params<T>(req: &JsonRpcRequest) -> std::result::Result<T, Box<JsonRpcResponse>>
where
    T: DeserializeOwned,
{
    match req.params.as_ref() {
        Some(params) => serde_json::from_value(params.clone()).map_err(|e| {
            Box::new(JsonRpcResponse::error(
                req.id.clone(),
                -32602,
                format!("Invalid params: {e}"),
            ))
        }),
        None => Err(Box::new(JsonRpcResponse::error(
            req.id.clone(),
            -32602,
            "Missing params",
        ))),
    }
}

fn success_response<T>(req: &JsonRpcRequest, result: T) -> JsonRpcResponse
where
    T: Serialize,
{
    match serde_json::to_value(result) {
        Ok(value) => JsonRpcResponse::success(req.id.clone(), value),
        Err(e) => JsonRpcResponse::error(
            req.id.clone(),
            -32603,
            format!("Failed to serialize response result: {e}"),
        ),
    }
}

fn handle_request(workspace_path: &str, store: &Store, req: &JsonRpcRequest) -> JsonRpcResponse {
    match req.method.as_str() {
        // Lifecycle
        "initialize" => {
            // Echo back the client's requested protocol version for compatibility.
            // Fall back to the baseline MCP spec version if not provided.
            let client_version = req
                .params
                .as_ref()
                .and_then(|p| p.get("protocolVersion"))
                .and_then(|v| v.as_str())
                .unwrap_or("2024-11-05");

            let result = InitializeResult {
                protocol_version: client_version.into(),
                capabilities: ServerCapabilities {
                    tools: Some(ToolsCapability {}),
                    resources: Some(ResourcesCapability {}),
                    prompts: Some(PromptsCapability {}),
                },
                server_info: ServerInfo {
                    name: format!("axon ({})", load_actual_model(store, workspace_path).name),
                    version: crate::VERSION.into(),
                },
            };
            success_response(req, result)
        }

        // Notifications: no response needed.
        "notifications/initialized" | "initialized" => success_response(req, json!({})),

        // Tools
        "tools/list" => {
            let mut all_tools = tools::list_tools();
            all_tools.extend(write_tools::list_write_tools());
            let result = ToolsListResult { tools: all_tools };
            success_response(req, result)
        }

        "tools/call" => {
            let params: ToolCallParams = match parse_params(req) {
                Ok(params) => params,
                Err(response) => return *response,
            };

            let result = if WRITE_TOOLS.contains(&params.name.as_str()) {
                write_tools::call_write_tool(workspace_path, store, &params.name, &params.arguments)
            } else {
                tools::call_tool(store, workspace_path, &params.name, &params.arguments)
            };
            success_response(req, result)
        }

        // Resources
        "resources/list" => {
            let result = ResourcesListResult {
                resources: resources::list_resources(store, workspace_path),
            };
            success_response(req, result)
        }

        "resources/read" => {
            let params: ResourceReadParams = match parse_params(req) {
                Ok(params) => params,
                Err(response) => return *response,
            };

            let result = resources::read_resource(store, workspace_path, &params.uri);
            success_response(req, result)
        }

        // Prompts
        "prompts/list" => {
            let result = PromptsListResult {
                prompts: prompts::list_prompts(),
            };
            success_response(req, result)
        }

        "prompts/get" => {
            let params: PromptGetParams = match parse_params(req) {
                Ok(params) => params,
                Err(response) => return *response,
            };

            let model = load_actual_model(store, workspace_path);
            match prompts::get_prompt(&model, store, workspace_path, &params.name) {
                Some(result) => success_response(req, result),
                None => JsonRpcResponse::error(
                    req.id.clone(),
                    -32602,
                    format!("Prompt not found: {}", params.name),
                ),
            }
        }

        // Ping (required by MCP spec)
        "ping" => success_response(req, json!({})),

        // Unknown
        method => JsonRpcResponse::error(
            req.id.clone(),
            -32601,
            format!("Method not found: {method}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::model::DomainModel;
    use serde_json::{Value, json};

    fn test_store() -> std::sync::Arc<Store> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("axon_stdio_test_{}_{}.db", std::process::id(), id));
        std::sync::Arc::new(Store::open(&path).unwrap())
    }

    fn make_request(method: &str, params: Option<Value>) -> JsonRpcRequest {
        JsonRpcRequest {
            _jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: method.into(),
            params,
        }
    }

    #[test]
    fn test_initialize_echoes_client_version() {
        let store = test_store();
        let req = make_request("initialize", Some(json!({"protocolVersion": "2024-11-05"})));
        let resp = handle_request("/tmp/test-stdio", &store, &req);
        let result = resp.result.unwrap();
        assert_eq!(result["protocolVersion"], "2024-11-05");
        assert!(
            result["serverInfo"]["name"]
                .as_str()
                .unwrap()
                .contains("axon")
        );
        assert!(result["capabilities"]["tools"].is_object());
        assert!(result["capabilities"]["resources"].is_object());
        assert!(result["capabilities"]["prompts"].is_object());
    }

    #[test]
    fn test_initialize_falls_back_to_baseline_version() {
        let store = test_store();
        let req = make_request("initialize", Some(json!({})));
        let resp = handle_request("/tmp/test-stdio", &store, &req);
        let result = resp.result.unwrap();
        assert_eq!(result["protocolVersion"], "2024-11-05");
    }

    #[test]
    fn test_ping_returns_empty_object() {
        let store = test_store();
        let req = make_request("ping", None);
        let resp = handle_request("/tmp/test-stdio", &store, &req);
        assert!(resp.error.is_none());
        assert_eq!(resp.result.unwrap(), json!({}));
    }

    #[test]
    fn test_unknown_method_returns_error() {
        let store = test_store();
        let req = make_request("nonexistent/method", None);
        let resp = handle_request("/tmp/test-stdio", &store, &req);
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32601);
    }

    #[test]
    fn test_tools_list_returns_all_tools() {
        let store = test_store();
        let req = make_request("tools/list", None);
        let resp = handle_request("/tmp/test-stdio", &store, &req);
        let result = resp.result.unwrap();
        let tools = result["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"rust_status"));
        assert!(names.contains(&"rust_readiness"));
        assert!(names.contains(&"rust_impact"));
        assert!(names.contains(&"rust_optimize"));
        assert!(names.contains(&"rust_scan"));
        assert!(names.contains(&"rust_diagnose"));
        assert!(names.contains(&"rust_history"));
        for omitted in ["rust_resolve", "rust_health", "rust_path", "rust_diff"] {
            assert!(!names.contains(&omitted));
        }
        assert!(!names.contains(&"architecture"));
        assert!(!names.contains(&"define"));
    }

    #[test]
    fn test_prompts_list_returns_guidelines() {
        let store = test_store();
        let req = make_request("prompts/list", None);
        let resp = handle_request("/tmp/test-stdio", &store, &req);
        let result = resp.result.unwrap();
        let prompts = result["prompts"].as_array().unwrap();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0]["name"], "axon_guidelines");
    }

    #[test]
    fn test_resources_list_returns_entries() {
        let store = test_store();
        let req = make_request("resources/list", None);
        let resp = handle_request("/tmp/test-stdio", &store, &req);
        let result = resp.result.unwrap();
        assert!(result["resources"].is_array());
    }

    #[test]
    fn test_tools_call_missing_params_returns_error() {
        let store = test_store();
        let req = make_request("tools/call", None);
        let resp = handle_request("/tmp/test-stdio", &store, &req);
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32602);
    }

    #[test]
    fn test_tools_call_architecture_health() {
        let store = test_store();
        let req = make_request(
            "tools/call",
            Some(json!({"name": "rust_status", "arguments": {"detail": "full"}})),
        );
        let resp = handle_request("/tmp/test-stdio", &store, &req);
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        let content = result["content"].as_array().unwrap();
        let text = content[0]["text"].as_str().unwrap();
        let arch: Value = serde_json::from_str(text).unwrap();
        assert!(arch["health"]["score"].is_number());
    }

    #[test]
    fn test_tools_call_routes_to_named_crate() {
        use std::fs;
        use std::sync::atomic::{AtomicU64, Ordering};

        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let workspace = std::env::temp_dir().join(format!(
            "axon_stdio_workspace_{}_{}",
            std::process::id(),
            id
        ));
        let member = workspace.join("member");
        fs::create_dir_all(workspace.join("src")).unwrap();
        fs::create_dir_all(member.join("src")).unwrap();
        fs::write(
            workspace.join("Cargo.toml"),
            "[package]\nname='root'\nversion='0.1.0'\nedition='2024'\n",
        )
        .unwrap();
        fs::write(
            member.join("Cargo.toml"),
            "[package]\nname='member'\nversion='0.1.0'\nedition='2024'\n",
        )
        .unwrap();

        let registry = CrateRegistry::open(&workspace).unwrap();
        let member_entry = registry.by_name("member").unwrap();
        let ws = member_entry.workspace_key();
        let model = DomainModel {
            name: "MemberModel".into(),
            description: "Member crate model".into(),
            bounded_contexts: vec![crate::domain::model::BoundedContext {
                name: "MemberContext".into(),
                description: "Member context".into(),
                module_path: "src".into(),
                ownership: Default::default(),
                aggregates: vec![],
                policies: vec![],
                read_models: vec![],
                entities: vec![],
                value_objects: vec![],
                services: vec![],
                repositories: vec![],
                events: vec![],
                modules: vec![],
                dependencies: vec![],
                api_endpoints: vec![],
            }],
            external_systems: vec![],
            architectural_decisions: vec![],
            ownership: Default::default(),
            rules: vec![],
            tech_stack: Default::default(),
            conventions: Default::default(),
            ast_edges: vec![],
            source_files: vec![],
            symbols: vec![],
            import_edges: vec![],
            call_edges: vec![],
            reference_edges: vec![],
        };
        member_entry.store.save_desired(&ws, &model).unwrap();
        member_entry.store.save_actual(&ws, &model).unwrap();
        member_entry.store.compute_drift(&ws).unwrap();

        let req = make_request(
            "tools/call",
            Some(
                json!({"name": "rust_status", "arguments": {"crate": "member", "detail": "full"}}),
            ),
        );
        let resp = handle_request_with_registry(&registry, &req);
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        let content = result["content"].as_array().unwrap();
        let text = content[0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["implemented"]["project"], "MemberModel");
    }

    #[test]
    fn test_prompts_get_nonexistent_returns_error() {
        let store = test_store();
        let req = make_request("prompts/get", Some(json!({"name": "nonexistent_prompt"})));
        let resp = handle_request("/tmp/test-stdio", &store, &req);
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32602);
    }
}
