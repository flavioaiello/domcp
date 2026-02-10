use serde_json::{json, Value};

use crate::domain::diff;
use crate::domain::model::*;
use crate::mcp::protocol::*;
use crate::store::Store;

/// Returns the list of write tools the DOMCP server exposes (bidirectional).
pub fn list_write_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "update_bounded_context".into(),
            description: "Create or update a bounded context in the domain model. \
                          Use this when analyzing a codebase to persist discovered contexts, \
                          or when refactoring the architecture."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Bounded context name" },
                    "description": { "type": "string" },
                    "module_path": { "type": "string", "description": "e.g. src/billing" },
                    "dependencies": {
                        "type": "array", "items": { "type": "string" },
                        "description": "Allowed dependencies to other contexts"
                    }
                },
                "required": ["name"]
            }),
        },
        ToolDefinition {
            name: "update_entity".into(),
            description: "Create or update an entity within a bounded context. \
                          Use when discovering entities in existing code or refactoring. \
                          Fields, methods, and invariants are merged (not replaced)."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "context": { "type": "string", "description": "Bounded context name" },
                    "name": { "type": "string", "description": "Entity name" },
                    "description": { "type": "string" },
                    "aggregate_root": { "type": "boolean" },
                    "fields": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": { "type": "string" },
                                "type": { "type": "string" },
                                "required": { "type": "boolean" },
                                "description": { "type": "string" }
                            },
                            "required": ["name", "type"]
                        }
                    },
                    "methods": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": { "type": "string" },
                                "description": { "type": "string" },
                                "parameters": {
                                    "type": "array",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "name": { "type": "string" },
                                            "type": { "type": "string" }
                                        },
                                        "required": ["name", "type"]
                                    }
                                },
                                "return_type": { "type": "string" }
                            },
                            "required": ["name"]
                        }
                    },
                    "invariants": {
                        "type": "array",
                        "items": { "type": "string" }
                    }
                },
                "required": ["context", "name"]
            }),
        },
        ToolDefinition {
            name: "update_service".into(),
            description: "Create or update a service within a bounded context. \
                          Use when discovering services in existing code."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "context": { "type": "string", "description": "Bounded context name" },
                    "name": { "type": "string", "description": "Service name" },
                    "description": { "type": "string" },
                    "kind": { "type": "string", "enum": ["domain", "application", "infrastructure"] },
                    "methods": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": { "type": "string" },
                                "description": { "type": "string" },
                                "parameters": {
                                    "type": "array",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "name": { "type": "string" },
                                            "type": { "type": "string" }
                                        },
                                        "required": ["name", "type"]
                                    }
                                },
                                "return_type": { "type": "string" }
                            },
                            "required": ["name"]
                        }
                    },
                    "dependencies": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["context", "name"]
            }),
        },
        ToolDefinition {
            name: "update_event".into(),
            description: "Create or update a domain event within a bounded context."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "context": { "type": "string", "description": "Bounded context name" },
                    "name": { "type": "string", "description": "Event name" },
                    "description": { "type": "string" },
                    "source": { "type": "string", "description": "Which entity emits this" },
                    "fields": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": { "type": "string" },
                                "type": { "type": "string" },
                                "description": { "type": "string" }
                            },
                            "required": ["name", "type"]
                        }
                    }
                },
                "required": ["context", "name"]
            }),
        },
        ToolDefinition {
            name: "remove_entity".into(),
            description: "Remove an entity from a bounded context."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "context": { "type": "string" },
                    "name": { "type": "string" }
                },
                "required": ["context", "name"]
            }),
        },
        ToolDefinition {
            name: "compare_model".into(),
            description: "Compare the current in-memory domain model against the persisted \
                          version. Returns a list of changes (added, removed, modified, moved) \
                          without generating code actions. Use this to review what changed \
                          before drafting a refactoring plan."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        ToolDefinition {
            name: "draft_refactoring_plan".into(),
            description: "Compare the current in-memory domain model against the persisted \
                          version and return a full refactoring plan with concrete \
                          code actions, file paths, priorities, and migration notes. \
                          Call this after reviewing the comparison to get actionable steps."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        ToolDefinition {
            name: "save_model".into(),
            description: "Persist the current domain model to the local store. \
                          Call this after applying changes and reviewing the refactoring plan."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
    ]
}

/// Dispatches a write tool call. Returns the result and the mutated model.
pub fn call_write_tool(
    model: &mut DomainModel,
    workspace_path: &str,
    store: &Store,
    name: &str,
    args: &Value,
) -> ToolCallResult {
    match name {
        "update_bounded_context" => {
            let ctx_name = arg_str(args, "name");
            if ctx_name.is_empty() {
                return error_result("'name' is required");
            }

            let existing = model
                .bounded_contexts
                .iter_mut()
                .find(|bc| bc.name.eq_ignore_ascii_case(&ctx_name));

            match existing {
                Some(bc) => {
                    // Update existing
                    if let Some(desc) = args.get("description").and_then(|v| v.as_str()) {
                        bc.description = desc.to_string();
                    }
                    if let Some(mp) = args.get("module_path").and_then(|v| v.as_str()) {
                        bc.module_path = mp.to_string();
                    }
                    if let Some(deps) = args.get("dependencies").and_then(|v| v.as_array()) {
                        bc.dependencies = deps
                            .iter()
                            .filter_map(|d| d.as_str().map(String::from))
                            .collect();
                    }
                    text_result(format!("Updated bounded context '{ctx_name}'"))
                }
                None => {
                    // Create new
                    model.bounded_contexts.push(BoundedContext {
                        name: ctx_name.clone(),
                        description: arg_str(args, "description"),
                        module_path: arg_str(args, "module_path"),
                        entities: vec![],
                        value_objects: vec![],
                        services: vec![],
                        repositories: vec![],
                        events: vec![],
                        dependencies: args
                            .get("dependencies")
                            .and_then(|v| v.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|d| d.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default(),
                    });
                    text_result(format!("Created bounded context '{ctx_name}'"))
                }
            }
        }

        "update_entity" => {
            let ctx_name = arg_str(args, "context");
            let entity_name = arg_str(args, "name");

            let bc = match model
                .bounded_contexts
                .iter_mut()
                .find(|bc| bc.name.eq_ignore_ascii_case(&ctx_name))
            {
                Some(bc) => bc,
                None => return error_result(format!("Bounded context '{ctx_name}' not found")),
            };

            let existing = bc
                .entities
                .iter_mut()
                .find(|e| e.name.eq_ignore_ascii_case(&entity_name));

            match existing {
                Some(entity) => {
                    // Merge updates
                    if let Some(desc) = args.get("description").and_then(|v| v.as_str()) {
                        entity.description = desc.to_string();
                    }
                    if let Some(agg) = args.get("aggregate_root").and_then(|v| v.as_bool()) {
                        entity.aggregate_root = agg;
                    }
                    if let Some(fields) = args.get("fields").and_then(|v| v.as_array()) {
                        merge_fields(&mut entity.fields, fields);
                    }
                    if let Some(methods) = args.get("methods").and_then(|v| v.as_array()) {
                        merge_methods(&mut entity.methods, methods);
                    }
                    if let Some(invariants) = args.get("invariants").and_then(|v| v.as_array()) {
                        for inv in invariants {
                            if let Some(s) = inv.as_str() {
                                if !entity.invariants.iter().any(|i| i == s) {
                                    entity.invariants.push(s.to_string());
                                }
                            }
                        }
                    }
                    text_result(format!("Updated entity '{entity_name}' in '{ctx_name}'"))
                }
                None => {
                    let entity = Entity {
                        name: entity_name.clone(),
                        description: arg_str(args, "description"),
                        aggregate_root: args
                            .get("aggregate_root")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                        fields: parse_fields(args.get("fields")),
                        methods: parse_methods(args.get("methods")),
                        invariants: args
                            .get("invariants")
                            .and_then(|v| v.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|i| i.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default(),
                    };
                    bc.entities.push(entity);
                    text_result(format!(
                        "Created entity '{entity_name}' in '{ctx_name}'"
                    ))
                }
            }
        }

        "update_service" => {
            let ctx_name = arg_str(args, "context");
            let svc_name = arg_str(args, "name");

            let bc = match model
                .bounded_contexts
                .iter_mut()
                .find(|bc| bc.name.eq_ignore_ascii_case(&ctx_name))
            {
                Some(bc) => bc,
                None => return error_result(format!("Bounded context '{ctx_name}' not found")),
            };

            let kind = match args.get("kind").and_then(|v| v.as_str()).unwrap_or("domain") {
                "application" => ServiceKind::Application,
                "infrastructure" => ServiceKind::Infrastructure,
                _ => ServiceKind::Domain,
            };

            let existing = bc
                .services
                .iter_mut()
                .find(|s| s.name.eq_ignore_ascii_case(&svc_name));

            match existing {
                Some(svc) => {
                    if let Some(desc) = args.get("description").and_then(|v| v.as_str()) {
                        svc.description = desc.to_string();
                    }
                    svc.kind = kind;
                    if let Some(deps) = args.get("dependencies").and_then(|v| v.as_array()) {
                        svc.dependencies = deps
                            .iter()
                            .filter_map(|d| d.as_str().map(String::from))
                            .collect();
                    }
                    if let Some(methods) = args.get("methods").and_then(|v| v.as_array()) {
                        merge_methods(&mut svc.methods, methods);
                    }
                    text_result(format!("Updated service '{svc_name}' in '{ctx_name}'"))
                }
                None => {
                    bc.services.push(Service {
                        name: svc_name.clone(),
                        description: arg_str(args, "description"),
                        kind,
                        methods: parse_methods(args.get("methods")),
                        dependencies: args
                            .get("dependencies")
                            .and_then(|v| v.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|d| d.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default(),
                    });
                    text_result(format!("Created service '{svc_name}' in '{ctx_name}'"))
                }
            }
        }

        "update_event" => {
            let ctx_name = arg_str(args, "context");
            let event_name = arg_str(args, "name");

            let bc = match model
                .bounded_contexts
                .iter_mut()
                .find(|bc| bc.name.eq_ignore_ascii_case(&ctx_name))
            {
                Some(bc) => bc,
                None => return error_result(format!("Bounded context '{ctx_name}' not found")),
            };

            let existing = bc
                .events
                .iter_mut()
                .find(|e| e.name.eq_ignore_ascii_case(&event_name));

            match existing {
                Some(evt) => {
                    if let Some(desc) = args.get("description").and_then(|v| v.as_str()) {
                        evt.description = desc.to_string();
                    }
                    if let Some(src) = args.get("source").and_then(|v| v.as_str()) {
                        evt.source = src.to_string();
                    }
                    if let Some(fields) = args.get("fields").and_then(|v| v.as_array()) {
                        merge_fields(&mut evt.fields, fields);
                    }
                    text_result(format!("Updated event '{event_name}' in '{ctx_name}'"))
                }
                None => {
                    bc.events.push(DomainEvent {
                        name: event_name.clone(),
                        description: arg_str(args, "description"),
                        fields: parse_fields(args.get("fields")),
                        source: arg_str(args, "source"),
                    });
                    text_result(format!("Created event '{event_name}' in '{ctx_name}'"))
                }
            }
        }

        "remove_entity" => {
            let ctx_name = arg_str(args, "context");
            let entity_name = arg_str(args, "name");

            let bc = match model
                .bounded_contexts
                .iter_mut()
                .find(|bc| bc.name.eq_ignore_ascii_case(&ctx_name))
            {
                Some(bc) => bc,
                None => return error_result(format!("Bounded context '{ctx_name}' not found")),
            };

            let before = bc.entities.len();
            bc.entities
                .retain(|e| !e.name.eq_ignore_ascii_case(&entity_name));

            if bc.entities.len() < before {
                text_result(format!(
                    "Removed entity '{entity_name}' from '{ctx_name}'"
                ))
            } else {
                error_result(format!(
                    "Entity '{entity_name}' not found in '{ctx_name}'"
                ))
            }
        }

        "compare_model" => {
            // Load the persisted model from the store and diff against current in-memory state
            match load_changes(store, workspace_path, model) {
                Ok(changes) => {
                    if changes.is_empty() {
                        text_result(
                            json!({
                                "status": "no_changes",
                                "message": "In-memory model matches persisted model"
                            })
                            .to_string(),
                        )
                    } else {
                        text_result(
                            json!({
                                "status": "changes_detected",
                                "change_count": changes.len(),
                                "changes": changes
                            })
                            .to_string(),
                        )
                    }
                }
                Err(e) => error_result(format!("Failed to compare models: {e}")),
            }
        }

        "draft_refactoring_plan" => {
            match load_changes(store, workspace_path, model) {
                Ok(changes) => {
                    if changes.is_empty() {
                        text_result(
                            json!({
                                "status": "no_changes",
                                "message": "In-memory model matches persisted model. Nothing to refactor."
                            })
                            .to_string(),
                        )
                    } else {
                        let plan = diff::plan_refactoring(&changes, &model.conventions);
                        text_result(serde_json::to_string(&plan).unwrap())
                    }
                }
                Err(e) => error_result(format!("Failed to load persisted model: {e}")),
            }
        }

        "save_model" => match store.save(workspace_path, model) {
            Ok(()) => text_result(format!("Domain model saved to store for workspace: {workspace_path}")),
            Err(e) => error_result(format!("Failed to save: {e}")),
        },

        _ => error_result(format!("Unknown write tool: {name}")),
    }
}

// ─── Helpers ───────────────────────────────────────────────────────────────

fn text_result(text: impl Into<String>) -> ToolCallResult {
    ToolCallResult {
        content: vec![ContentBlock::Text { text: text.into() }],
        is_error: None,
    }
}

fn error_result(msg: impl Into<String>) -> ToolCallResult {
    ToolCallResult {
        content: vec![ContentBlock::Text { text: msg.into() }],
        is_error: Some(true),
    }
}

/// Load persisted model and compute changes against the in-memory model.
fn load_changes(
    store: &Store,
    workspace_path: &str,
    model: &DomainModel,
) -> anyhow::Result<Vec<diff::ModelChange>> {
    let persisted = match store.load(workspace_path)? {
        Some(m) => m,
        None => DomainModel::empty(workspace_path),
    };
    Ok(diff::diff_models(&persisted, model))
}

fn arg_str(args: &Value, key: &str) -> String {
    args.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn parse_fields(val: Option<&Value>) -> Vec<Field> {
    val.and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|f| {
                    Some(Field {
                        name: f.get("name")?.as_str()?.to_string(),
                        field_type: f.get("type")?.as_str()?.to_string(),
                        required: f
                            .get("required")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                        description: f
                            .get("description")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_methods(val: Option<&Value>) -> Vec<Method> {
    val.and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    Some(Method {
                        name: m.get("name")?.as_str()?.to_string(),
                        description: m
                            .get("description")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        parameters: parse_fields(m.get("parameters")),
                        return_type: m
                            .get("return_type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn merge_fields(existing: &mut Vec<Field>, new_fields: &[Value]) {
    for f in new_fields {
        let name = match f.get("name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => continue,
        };
        if let Some(existing_f) = existing.iter_mut().find(|ef| ef.name == name) {
            if let Some(t) = f.get("type").and_then(|v| v.as_str()) {
                existing_f.field_type = t.to_string();
            }
            if let Some(r) = f.get("required").and_then(|v| v.as_bool()) {
                existing_f.required = r;
            }
            if let Some(d) = f.get("description").and_then(|v| v.as_str()) {
                existing_f.description = d.to_string();
            }
        } else if let Some(field) = parse_fields(Some(&json!([f]))).into_iter().next() {
            existing.push(field);
        }
    }
}

fn merge_methods(existing: &mut Vec<Method>, new_methods: &[Value]) {
    for m in new_methods {
        let name = match m.get("name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => continue,
        };
        if let Some(existing_m) = existing.iter_mut().find(|em| em.name == name) {
            if let Some(d) = m.get("description").and_then(|v| v.as_str()) {
                existing_m.description = d.to_string();
            }
            if let Some(rt) = m.get("return_type").and_then(|v| v.as_str()) {
                existing_m.return_type = rt.to_string();
            }
        } else if let Some(method) = parse_methods(Some(&json!([m]))).into_iter().next() {
            existing.push(method);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use std::env::temp_dir;

    fn test_store() -> Store {
        let path = temp_dir().join(format!("domcp_wt_test_{}.db", std::process::id()));
        Store::open(&path).unwrap()
    }

    fn test_model() -> DomainModel {
        DomainModel {
            name: "TestProject".into(),
            description: "Test".into(),
            bounded_contexts: vec![BoundedContext {
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
                services: vec![],
                repositories: vec![],
                events: vec![],
                dependencies: vec![],
            }],
            rules: vec![],
            tech_stack: TechStack::default(),
            conventions: Conventions::default(),
        }
    }

    #[test]
    fn test_list_write_tools_count() {
        assert_eq!(list_write_tools().len(), 8);
    }

    #[test]
    fn test_update_entity_add_field() {
        let mut model = test_model();
        let store = test_store();
        let result = call_write_tool(
            &mut model,
            "/tmp/test-ws",
            &store,
            "update_entity",
            &json!({
                "context": "Identity",
                "name": "User",
                "fields": [{"name": "email", "type": "String", "required": true}]
            }),
        );
        assert!(result.is_error.is_none());
        let user = &model.bounded_contexts[0].entities[0];
        assert_eq!(user.fields.len(), 2);
        assert_eq!(user.fields[1].name, "email");
    }

    #[test]
    fn test_update_entity_merge_existing_field() {
        let mut model = test_model();
        let store = test_store();
        let result = call_write_tool(
            &mut model,
            "/tmp/test-ws",
            &store,
            "update_entity",
            &json!({
                "context": "Identity",
                "name": "User",
                "fields": [{"name": "id", "type": "Uuid"}]
            }),
        );
        assert!(result.is_error.is_none());
        let user = &model.bounded_contexts[0].entities[0];
        assert_eq!(user.fields.len(), 1);
        assert_eq!(user.fields[0].field_type, "Uuid");
    }

    #[test]
    fn test_create_new_entity() {
        let mut model = test_model();
        let store = test_store();
        let result = call_write_tool(
            &mut model,
            "/tmp/test-ws",
            &store,
            "update_entity",
            &json!({
                "context": "Identity",
                "name": "Role",
                "description": "A role assignment",
                "aggregate_root": false,
                "fields": [{"name": "name", "type": "String"}]
            }),
        );
        assert!(result.is_error.is_none());
        assert_eq!(model.bounded_contexts[0].entities.len(), 2);
        assert_eq!(model.bounded_contexts[0].entities[1].name, "Role");
    }

    #[test]
    fn test_update_entity_context_not_found() {
        let mut model = test_model();
        let store = test_store();
        let result = call_write_tool(
            &mut model,
            "/tmp/test-ws",
            &store,
            "update_entity",
            &json!({"context": "Nonexistent", "name": "Foo"}),
        );
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_create_bounded_context() {
        let mut model = test_model();
        let store = test_store();
        let result = call_write_tool(
            &mut model,
            "/tmp/test-ws",
            &store,
            "update_bounded_context",
            &json!({
                "name": "Billing",
                "description": "Billing context",
                "module_path": "src/billing",
                "dependencies": ["Identity"]
            }),
        );
        assert!(result.is_error.is_none());
        assert_eq!(model.bounded_contexts.len(), 2);
        assert_eq!(model.bounded_contexts[1].name, "Billing");
        assert_eq!(model.bounded_contexts[1].dependencies, vec!["Identity"]);
    }

    #[test]
    fn test_update_existing_bounded_context() {
        let mut model = test_model();
        let store = test_store();
        let result = call_write_tool(
            &mut model,
            "/tmp/test-ws",
            &store,
            "update_bounded_context",
            &json!({
                "name": "Identity",
                "description": "Updated description"
            }),
        );
        assert!(result.is_error.is_none());
        assert_eq!(model.bounded_contexts.len(), 1);
        assert_eq!(model.bounded_contexts[0].description, "Updated description");
    }

    #[test]
    fn test_remove_entity() {
        let mut model = test_model();
        let store = test_store();
        let result = call_write_tool(
            &mut model,
            "/tmp/test-ws",
            &store,
            "remove_entity",
            &json!({"context": "Identity", "name": "User"}),
        );
        assert!(result.is_error.is_none());
        assert_eq!(model.bounded_contexts[0].entities.len(), 0);
    }

    #[test]
    fn test_remove_entity_not_found() {
        let mut model = test_model();
        let store = test_store();
        let result = call_write_tool(
            &mut model,
            "/tmp/test-ws",
            &store,
            "remove_entity",
            &json!({"context": "Identity", "name": "NotHere"}),
        );
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_update_service() {
        let mut model = test_model();
        let store = test_store();
        let result = call_write_tool(
            &mut model,
            "/tmp/test-ws",
            &store,
            "update_service",
            &json!({
                "context": "Identity",
                "name": "AuthService",
                "kind": "application",
                "description": "Handles authentication"
            }),
        );
        assert!(result.is_error.is_none());
        assert_eq!(model.bounded_contexts[0].services.len(), 1);
        assert_eq!(
            model.bounded_contexts[0].services[0].description,
            "Handles authentication"
        );
    }

    #[test]
    fn test_update_event() {
        let mut model = test_model();
        let store = test_store();
        let result = call_write_tool(
            &mut model,
            "/tmp/test-ws",
            &store,
            "update_event",
            &json!({
                "context": "Identity",
                "name": "UserRegistered",
                "source": "User",
                "fields": [{"name": "user_id", "type": "UserId"}]
            }),
        );
        assert!(result.is_error.is_none());
        assert_eq!(model.bounded_contexts[0].events.len(), 1);
        assert_eq!(model.bounded_contexts[0].events[0].name, "UserRegistered");
    }

    #[test]
    fn test_unknown_write_tool() {
        let mut model = test_model();
        let store = test_store();
        let result = call_write_tool(&mut model, "/tmp/test-ws", &store, "nonexistent", &json!({}));
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn test_save_and_compare_no_changes() {
        let mut model = test_model();
        let store = test_store();
        let ws = "/tmp/test-compare-none";
        call_write_tool(&mut model, ws, &store, "save_model", &json!({}));
        let result = call_write_tool(&mut model, ws, &store, "compare_model", &json!({}));
        let text = match &result.content[0] { ContentBlock::Text { text } => text };
        assert!(text.contains("no_changes"));
    }

    #[test]
    fn test_compare_detects_new_entity() {
        let mut model = test_model();
        let store = test_store();
        let ws = "/tmp/test-compare-ent";
        call_write_tool(&mut model, ws, &store, "save_model", &json!({}));
        call_write_tool(
            &mut model, ws, &store, "update_entity",
            &json!({"context": "Identity", "name": "Role", "aggregate_root": false}),
        );
        let result = call_write_tool(&mut model, ws, &store, "compare_model", &json!({}));
        let text = match &result.content[0] { ContentBlock::Text { text } => text };
        assert!(text.contains("changes_detected"));
        assert!(text.contains("Role"));
    }

    #[test]
    fn test_draft_refactoring_plan_produces_actions() {
        let mut model = test_model();
        let store = test_store();
        let ws = "/tmp/test-plan";
        // Use non-default conventions with a pattern so plan generates file paths
        model.conventions = Conventions {
            file_structure: FileStructure {
                pattern: "src/{context}/{layer}/{type}.rs".into(),
                layers: vec!["domain".into(), "application".into()],
            },
            ..Default::default()
        };
        call_write_tool(&mut model, ws, &store, "save_model", &json!({}));
        call_write_tool(
            &mut model, ws, &store, "update_entity",
            &json!({"context": "Identity", "name": "Role"}),
        );
        let result = call_write_tool(&mut model, ws, &store, "draft_refactoring_plan", &json!({}));
        let text = match &result.content[0] { ContentBlock::Text { text } => text };
        assert!(text.contains("code_actions"));
        assert!(text.contains("role"));
    }

    #[test]
    fn test_draft_refactoring_plan_no_changes() {
        let mut model = test_model();
        let store = test_store();
        let ws = "/tmp/test-plan-none";
        call_write_tool(&mut model, ws, &store, "save_model", &json!({}));
        let result = call_write_tool(&mut model, ws, &store, "draft_refactoring_plan", &json!({}));
        let text = match &result.content[0] { ContentBlock::Text { text } => text };
        assert!(text.contains("no_changes"));
    }

    #[test]
    fn test_update_service_merges_methods() {
        let mut model = test_model();
        let store = test_store();
        // First create a service with a method
        call_write_tool(
            &mut model, "/tmp/test-ws", &store, "update_service",
            &json!({
                "context": "Identity",
                "name": "AuthService",
                "kind": "application",
                "methods": [{"name": "login", "return_type": "Token"}]
            }),
        );
        assert_eq!(model.bounded_contexts[0].services[0].methods.len(), 1);
        // Update with a new method — should merge, not replace
        call_write_tool(
            &mut model, "/tmp/test-ws", &store, "update_service",
            &json!({
                "context": "Identity",
                "name": "AuthService",
                "methods": [{"name": "logout", "return_type": "void"}]
            }),
        );
        assert_eq!(model.bounded_contexts[0].services[0].methods.len(), 2);
    }
}
