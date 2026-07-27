/// MCP JSON-RPC protocol types (SDK-compatible, spec 2025-03-26)
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

// ─── JSON-RPC Envelope ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcRequest {
    #[serde(rename = "jsonrpc")]
    pub _jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcResponse {
    pub fn success(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Option<Value>, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

// ─── MCP Initialize ────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Serialize)]
pub struct ServerCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourcesCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompts: Option<PromptsCapability>,
}

#[derive(Debug, Serialize)]
pub struct ToolsCapability {}

#[derive(Debug, Serialize)]
pub struct ResourcesCapability {}

#[derive(Debug, Serialize)]
pub struct PromptsCapability {}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    pub server_info: ServerInfo,
}

// ─── Tools ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Debug, Serialize)]
pub struct ToolsListResult {
    pub tools: Vec<ToolDefinition>,
}

#[derive(Debug, Deserialize)]
pub struct ToolCallParams {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

#[derive(Debug, Serialize)]
pub struct ToolCallResult {
    pub content: Vec<ContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "isError")]
    pub is_error: Option<bool>,
}

pub fn text_tool_result(text: impl Into<String>) -> ToolCallResult {
    ToolCallResult {
        content: vec![ContentBlock::Text { text: text.into() }],
        is_error: None,
    }
}

pub fn error_tool_result(message: impl Into<String>) -> ToolCallResult {
    ToolCallResult {
        content: vec![ContentBlock::Text {
            text: message.into(),
        }],
        is_error: Some(true),
    }
}

pub fn json_tool_result(value: Value) -> ToolCallResult {
    text_tool_result(value.to_string())
}

pub fn json_error_tool_result(value: Value) -> ToolCallResult {
    ToolCallResult {
        content: vec![ContentBlock::Text {
            text: value.to_string(),
        }],
        is_error: Some(true),
    }
}

pub fn with_reasoning_context(
    mut payload: Value,
    proof: Option<Value>,
    evidence: Option<Value>,
    limitations: Vec<String>,
    provenance: Option<Value>,
) -> Value {
    if !payload.is_object() {
        payload = json!({ "result": payload });
    }

    let Some(object) = payload.as_object_mut() else {
        return payload;
    };

    if let Some(proof) = proof {
        object.insert("proof".into(), proof);
    }
    if let Some(evidence) = evidence {
        object.insert("evidence".into(), evidence);
    }
    if !limitations.is_empty() {
        object.insert(
            "limitations".into(),
            Value::Array(limitations.into_iter().map(Value::String).collect()),
        );
    }
    if let Some(provenance) = provenance {
        object.insert("provenance".into(), provenance);
    }

    payload
}

pub fn with_workspace_context_schema(mut schema: Value) -> Value {
    if !schema.is_object() {
        schema = json!({ "type": "object", "properties": {} });
    }

    let Some(object) = schema.as_object_mut() else {
        return schema;
    };
    object
        .entry("type")
        .or_insert_with(|| Value::String("object".into()));
    if object
        .get("required")
        .and_then(|required| required.as_array())
        .is_some_and(|required| required.is_empty())
    {
        object.remove("required");
    }
    let properties = object.entry("properties").or_insert_with(|| json!({}));
    let Some(properties) = properties.as_object_mut() else {
        return schema;
    };

    properties.entry("workspace_path").or_insert_with(|| {
        json!({
            "type": "string",
            "description": "Absolute path to the Cargo workspace/package root for this tool call. In daemon mode this is the preferred routing context."
        })
    });
    properties.entry("file_path").or_insert_with(|| {
        json!({
            "type": "string",
            "description": "Absolute path to a file inside the target workspace; Axon infers the Cargo workspace from it."
        })
    });
    properties.entry("crate").or_insert_with(|| {
        json!({
            "type": "string",
            "description": "Crate name within the selected workspace for multi-crate workspaces. Requires workspace_path, file_path, or a legacy session default."
        })
    });
    properties.entry("crate_name").or_insert_with(|| {
        json!({
            "type": "string",
            "description": "Alias for crate."
        })
    });

    let workspace_context_requirement = json!({
        "anyOf": [
            { "required": ["workspace_path"] },
            { "required": ["file_path"] }
        ]
    });
    match object.get_mut("allOf") {
        Some(Value::Array(all_of)) => {
            if !all_of.contains(&workspace_context_requirement) {
                all_of.push(workspace_context_requirement);
            }
        }
        Some(existing) => {
            let existing = std::mem::take(existing);
            object.insert(
                "allOf".into(),
                Value::Array(vec![existing, workspace_context_requirement]),
            );
        }
        None => {
            object.insert(
                "allOf".into(),
                Value::Array(vec![workspace_context_requirement]),
            );
        }
    }

    schema
}

pub fn with_workspace_context_description(description: impl Into<String>) -> String {
    format!(
        "{} Requires workspace_path (Cargo workspace/package root) or file_path (file inside the target workspace) in every tool call.",
        description.into()
    )
}

// ─── Resources ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ResourceDefinition {
    pub uri: String,
    pub name: String,
    pub description: String,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
}

#[derive(Debug, Serialize)]
pub struct ResourcesListResult {
    pub resources: Vec<ResourceDefinition>,
}

#[derive(Debug, Deserialize)]
pub struct ResourceReadParams {
    pub uri: String,
}

#[derive(Debug, Serialize)]
pub struct ResourceReadResult {
    pub contents: Vec<ResourceContent>,
}

#[derive(Debug, Serialize)]
pub struct ResourceContent {
    pub uri: String,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    pub text: String,
}

// ─── Prompts ───────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct PromptDefinition {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arguments: Vec<PromptArgument>,
}

#[derive(Debug, Serialize)]
pub struct PromptArgument {
    pub name: String,
    pub description: String,
    pub required: bool,
}

#[derive(Debug, Serialize)]
pub struct PromptsListResult {
    pub prompts: Vec<PromptDefinition>,
}

#[derive(Debug, Deserialize)]
pub struct PromptGetParams {
    pub name: String,
    #[serde(default, rename = "arguments")]
    pub _arguments: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct PromptGetResult {
    pub description: String,
    pub messages: Vec<PromptMessage>,
}

#[derive(Debug, Serialize)]
pub struct PromptMessage {
    pub role: String,
    pub content: ContentBlock,
}

// ─── Content ───────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
}
