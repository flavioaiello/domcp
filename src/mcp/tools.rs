use serde_json::{json, Value};

use crate::domain::model::DomainModel;
use crate::domain::registry::DomainRegistry;
use crate::domain::to_snake;
use crate::mcp::protocol::*;

/// Returns the list of tools the DOMCP server exposes.
pub fn list_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "get_architecture_overview".into(),
            description: "Returns a full architecture overview including bounded contexts, \
                          entities, services, events, rules, and conventions. \
                          Use this before writing any new code to understand the system structure."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        ToolDefinition {
            name: "get_bounded_context".into(),
            description: "Returns detailed information about a specific bounded context, \
                          including its entities, value objects, services, repositories, \
                          domain events, and allowed dependencies."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Name of the bounded context"
                    }
                },
                "required": ["name"]
            }),
        },
        ToolDefinition {
            name: "get_entity".into(),
            description: "Returns the full specification of a domain entity including fields, \
                          methods, invariants, and whether it is an aggregate root. \
                          Use this when implementing or modifying an entity."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Name of the entity"
                    }
                },
                "required": ["name"]
            }),
        },
        ToolDefinition {
            name: "get_service_spec".into(),
            description: "Returns the specification for a domain/application/infrastructure \
                          service including its methods, dependencies, and layer classification."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Name of the service"
                    }
                },
                "required": ["name"]
            }),
        },
        ToolDefinition {
            name: "validate_dependency".into(),
            description: "Checks whether a dependency from one bounded context to another \
                          is allowed per the architectural rules. Returns allowed/denied \
                          with explanation."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "from_context": {
                        "type": "string",
                        "description": "Source bounded context name"
                    },
                    "to_context": {
                        "type": "string",
                        "description": "Target bounded context name"
                    }
                },
                "required": ["from_context", "to_context"]
            }),
        },
        ToolDefinition {
            name: "get_architectural_rules".into(),
            description: "Returns all architectural rules and constraints that code must adhere to. \
                          Check these rules before generating or modifying code."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        ToolDefinition {
            name: "get_conventions".into(),
            description: "Returns naming conventions, file structure patterns, error handling \
                          strategy, and testing conventions for the project."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        ToolDefinition {
            name: "suggest_file_path".into(),
            description: "Given a type category (entity, service, repository, event, value_object) \
                          and a bounded context, suggests the correct file path following project \
                          conventions."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "context": {
                        "type": "string",
                        "description": "Bounded context name"
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["entity", "value_object", "service", "repository", "event"],
                        "description": "Type of domain artifact"
                    },
                    "name": {
                        "type": "string",
                        "description": "Name of the artifact"
                    }
                },
                "required": ["context", "kind", "name"]
            }),
        },
    ]
}

/// Dispatches a tool call and returns the result.
pub fn call_tool(model: &DomainModel, name: &str, args: &Value) -> ToolCallResult {
    let registry = DomainRegistry::new(model);

    match name {
        "get_architecture_overview" => {
            let summary = registry.architecture_summary();
            text_result(summary)
        }

        "get_bounded_context" => {
            let ctx_name = args["name"].as_str().unwrap_or("");
            match registry.find_context(ctx_name) {
                Some(bc) => text_result(serde_json::to_string(bc).unwrap()),
                None => error_result(format!(
                    "Bounded context '{}' not found. Available: {}",
                    ctx_name,
                    registry.context_names().join(", ")
                )),
            }
        }

        "get_entity" => {
            let entity_name = args["name"].as_str().unwrap_or("");
            match registry.find_entity(entity_name) {
                Some((bc, entity)) => {
                    let result = json!({
                        "bounded_context": bc.name,
                        "module_path": bc.module_path,
                        "entity": entity,
                    });
                    text_result(serde_json::to_string(&result).unwrap())
                }
                None => error_result(format!("Entity '{}' not found in any bounded context", entity_name)),
            }
        }

        "get_service_spec" => {
            let svc_name = args["name"].as_str().unwrap_or("");
            match registry.find_service(svc_name) {
                Some((bc, svc)) => {
                    let result = json!({
                        "bounded_context": bc.name,
                        "service": svc,
                    });
                    text_result(serde_json::to_string(&result).unwrap())
                }
                None => error_result(format!("Service '{}' not found", svc_name)),
            }
        }

        "validate_dependency" => {
            let from = args["from_context"].as_str().unwrap_or("");
            let to = args["to_context"].as_str().unwrap_or("");

            match registry.find_context(from) {
                Some(bc) => {
                    let allowed = bc.dependencies.iter().any(|d| d.eq_ignore_ascii_case(to));
                    let result = json!({
                        "from": from,
                        "to": to,
                        "allowed": allowed,
                        "explanation": if allowed {
                            format!("'{}' is an allowed dependency of '{}'", to, from)
                        } else {
                            format!(
                                "'{}' is NOT allowed to depend on '{}'. Allowed dependencies: {}",
                                from,
                                to,
                                if bc.dependencies.is_empty() {
                                    "none".to_string()
                                } else {
                                    bc.dependencies.join(", ")
                                }
                            )
                        }
                    });
                    text_result(serde_json::to_string(&result).unwrap())
                }
                None => error_result(format!("Bounded context '{}' not found", from)),
            }
        }

        "get_architectural_rules" => {
            text_result(serde_json::to_string(&model.rules).unwrap())
        }

        "get_conventions" => {
            text_result(serde_json::to_string(&model.conventions).unwrap())
        }

        "suggest_file_path" => {
            let context = args["context"].as_str().unwrap_or("");
            let kind = args["kind"].as_str().unwrap_or("");
            let artifact_name = args["name"].as_str().unwrap_or("");
            let pattern = &model.conventions.file_structure.pattern;

            // Map artifact kind to the architectural layer
            let layer = match kind {
                "entity" | "value_object" | "event" => "domain",
                "service" => "application",
                "repository" => "infrastructure",
                other => other,
            };

            if pattern.is_empty() {
                return text_result(format!(
                    "No file structure pattern configured. Suggested: src/{}/{}/{}.rs",
                    to_snake(context),
                    layer,
                    to_snake(artifact_name)
                ));
            }

            let path = pattern
                .replace("{context}", &to_snake(context))
                .replace("{layer}", layer)
                .replace("{type}", &to_snake(artifact_name));

            text_result(json!({
                "suggested_path": path,
                "pattern": pattern,
            }).to_string())
        }

        _ => error_result(format!("Unknown tool: {}", name)),
    }
}

fn text_result(text: String) -> ToolCallResult {
    ToolCallResult {
        content: vec![ContentBlock::Text { text }],
        is_error: None,
    }
}

fn error_result(msg: String) -> ToolCallResult {
    ToolCallResult {
        content: vec![ContentBlock::Text { text: msg }],
        is_error: Some(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::model::*;

    fn test_model() -> DomainModel {
        DomainModel {
            name: "TestProject".into(),
            description: "Test".into(),
            bounded_contexts: vec![
                BoundedContext {
                    name: "Identity".into(),
                    description: "Auth context".into(),
                    module_path: "src/identity".into(),
                    entities: vec![Entity {
                        name: "User".into(),
                        description: "A user".into(),
                        aggregate_root: true,
                        fields: vec![Field {
                            name: "id".into(),
                            field_type: "UserId".into(),
                            required: true,
                            description: "".into(),
                        }],
                        methods: vec![],
                        invariants: vec!["Email must be unique".into()],
                    }],
                    value_objects: vec![],
                    services: vec![Service {
                        name: "AuthService".into(),
                        description: "Handles auth".into(),
                        kind: ServiceKind::Application,
                        methods: vec![],
                        dependencies: vec![],
                    }],
                    repositories: vec![],
                    events: vec![],
                    dependencies: vec![],
                },
                BoundedContext {
                    name: "Billing".into(),
                    description: "Billing context".into(),
                    module_path: "src/billing".into(),
                    entities: vec![],
                    value_objects: vec![],
                    services: vec![],
                    repositories: vec![],
                    events: vec![],
                    dependencies: vec!["Identity".into()],
                },
            ],
            rules: vec![ArchitecturalRule {
                id: "LAYER-001".into(),
                description: "Domain must not depend on infra".into(),
                severity: Severity::Error,
                scope: "domain".into(),
            }],
            tech_stack: TechStack::default(),
            conventions: Conventions {
                file_structure: FileStructure {
                    pattern: "src/{context}/{layer}/{type}.rs".into(),
                    layers: vec!["domain".into(), "application".into()],
                },
                ..Default::default()
            },
        }
    }

    #[test]
    fn test_get_entity_found() {
        let model = test_model();
        let result = call_tool(&model, "get_entity", &json!({"name": "User"}));
        assert!(result.is_error.is_none());
        let text = match &result.content[0] {
            ContentBlock::Text { text } => text,
        };
        assert!(text.contains("\"aggregate_root\":true"));
        assert!(text.contains("Identity"));
    }

    #[test]
    fn test_get_entity_not_found() {
        let model = test_model();
        let result = call_tool(&model, "get_entity", &json!({"name": "Nonexistent"}));
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_get_entity_case_insensitive() {
        let model = test_model();
        let result = call_tool(&model, "get_entity", &json!({"name": "user"}));
        assert!(result.is_error.is_none());
    }

    #[test]
    fn test_validate_dependency_allowed() {
        let model = test_model();
        let result = call_tool(
            &model,
            "validate_dependency",
            &json!({"from_context": "Billing", "to_context": "Identity"}),
        );
        let text = match &result.content[0] {
            ContentBlock::Text { text } => text,
        };
        assert!(text.contains("\"allowed\":true"));
    }

    #[test]
    fn test_validate_dependency_denied() {
        let model = test_model();
        let result = call_tool(
            &model,
            "validate_dependency",
            &json!({"from_context": "Identity", "to_context": "Billing"}),
        );
        let text = match &result.content[0] {
            ContentBlock::Text { text } => text,
        };
        assert!(text.contains("\"allowed\":false"));
    }

    #[test]
    fn test_suggest_file_path_entity_maps_to_domain_layer() {
        let model = test_model();
        let result = call_tool(
            &model,
            "suggest_file_path",
            &json!({"context": "Identity", "kind": "entity", "name": "User"}),
        );
        let text = match &result.content[0] {
            ContentBlock::Text { text } => text,
        };
        assert!(text.contains("src/identity/domain/user.rs"));
    }

    #[test]
    fn test_suggest_file_path_repository_maps_to_infrastructure() {
        let model = test_model();
        let result = call_tool(
            &model,
            "suggest_file_path",
            &json!({"context": "Identity", "kind": "repository", "name": "UserRepository"}),
        );
        let text = match &result.content[0] {
            ContentBlock::Text { text } => text,
        };
        assert!(text.contains("src/identity/infrastructure/user_repository.rs"));
    }

    #[test]
    fn test_get_architectural_rules() {
        let model = test_model();
        let result = call_tool(&model, "get_architectural_rules", &json!({}));
        let text = match &result.content[0] {
            ContentBlock::Text { text } => text,
        };
        assert!(text.contains("LAYER-001"));
    }

    #[test]
    fn test_unknown_tool() {
        let model = test_model();
        let result = call_tool(&model, "nonexistent_tool", &json!({}));
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_list_tools_count() {
        let tools = list_tools();
        assert_eq!(tools.len(), 8);
    }
}
