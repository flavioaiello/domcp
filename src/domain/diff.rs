use serde::{Deserialize, Serialize};
use serde_json::json;

use super::model::*;
use super::to_snake;

/// Represents a change to the domain model for refactoring planning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelChange {
    pub kind: ChangeKind,
    pub path: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Removed,
    Modified,
    Moved,
}

/// A refactoring plan derived from model changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefactoringPlan {
    pub model_changes: Vec<ModelChange>,
    pub code_actions: Vec<CodeAction>,
    pub migration_notes: Vec<String>,
}

/// A concrete code action to perform.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeAction {
    pub action: ActionKind,
    pub file_path: String,
    pub description: String,
    pub priority: Priority,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    CreateFile,
    ModifyFile,
    DeleteFile,
    MoveFile,
    UpdateImports,
    AddTest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Critical,
    High,
    Medium,
    Low,
}

/// Diff two domain models and produce a structured change set.
pub fn diff_models(old: &DomainModel, new: &DomainModel) -> Vec<ModelChange> {
    let mut changes = Vec::new();

    // Diff bounded contexts
    for new_bc in &new.bounded_contexts {
        match old
            .bounded_contexts
            .iter()
            .find(|bc| bc.name.eq_ignore_ascii_case(&new_bc.name))
        {
            None => {
                changes.push(ModelChange {
                    kind: ChangeKind::Added,
                    path: format!("bounded_contexts.{}", new_bc.name),
                    description: format!("New bounded context: {}", new_bc.name),
                    before: None,
                    after: Some(json!({"name": new_bc.name, "module": new_bc.module_path})),
                });
            }
            Some(old_bc) => {
                diff_context(old_bc, new_bc, &mut changes);
            }
        }
    }

    // Detect removed contexts
    for old_bc in &old.bounded_contexts {
        if !new
            .bounded_contexts
            .iter()
            .any(|bc| bc.name.eq_ignore_ascii_case(&old_bc.name))
        {
            changes.push(ModelChange {
                kind: ChangeKind::Removed,
                path: format!("bounded_contexts.{}", old_bc.name),
                description: format!("Removed bounded context: {}", old_bc.name),
                before: Some(json!({"name": old_bc.name})),
                after: None,
            });
        }
    }

    // Diff rules
    for new_rule in &new.rules {
        if !old.rules.iter().any(|r| r.id == new_rule.id) {
            changes.push(ModelChange {
                kind: ChangeKind::Added,
                path: format!("rules.{}", new_rule.id),
                description: format!("New rule: {} — {}", new_rule.id, new_rule.description),
                before: None,
                after: Some(serde_json::to_value(new_rule).unwrap()),
            });
        }
    }
    for old_rule in &old.rules {
        if !new.rules.iter().any(|r| r.id == old_rule.id) {
            changes.push(ModelChange {
                kind: ChangeKind::Removed,
                path: format!("rules.{}", old_rule.id),
                description: format!("Removed rule: {}", old_rule.id),
                before: Some(serde_json::to_value(old_rule).unwrap()),
                after: None,
            });
        }
    }

    // Modified rules
    for new_rule in &new.rules {
        if let Some(old_rule) = old.rules.iter().find(|r| r.id == new_rule.id) {
            if old_rule.description != new_rule.description
                || format!("{:?}", old_rule.severity) != format!("{:?}", new_rule.severity)
            {
                changes.push(ModelChange {
                    kind: ChangeKind::Modified,
                    path: format!("rules.{}", new_rule.id),
                    description: format!("Modified rule: {}", new_rule.id),
                    before: Some(serde_json::to_value(old_rule).unwrap()),
                    after: Some(serde_json::to_value(new_rule).unwrap()),
                });
            }
        }
    }

    changes
}

fn diff_context(old: &BoundedContext, new: &BoundedContext, changes: &mut Vec<ModelChange>) {
    let ctx = &new.name;

    // Module path change (= move)
    if old.module_path != new.module_path && !new.module_path.is_empty() {
        changes.push(ModelChange {
            kind: ChangeKind::Moved,
            path: format!("{ctx}.module_path"),
            description: format!(
                "Context '{}' moved: {} → {}",
                ctx, old.module_path, new.module_path
            ),
            before: Some(json!(old.module_path)),
            after: Some(json!(new.module_path)),
        });
    }

    // Entities
    for new_e in &new.entities {
        match old
            .entities
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(&new_e.name))
        {
            None => {
                changes.push(ModelChange {
                    kind: ChangeKind::Added,
                    path: format!("{ctx}.entities.{}", new_e.name),
                    description: format!("New entity '{}' in context '{}'", new_e.name, ctx),
                    before: None,
                    after: Some(serde_json::to_value(new_e).unwrap()),
                });
            }
            Some(old_e) => {
                diff_entity(ctx, old_e, new_e, changes);
            }
        }
    }
    for old_e in &old.entities {
        if !new
            .entities
            .iter()
            .any(|e| e.name.eq_ignore_ascii_case(&old_e.name))
        {
            changes.push(ModelChange {
                kind: ChangeKind::Removed,
                path: format!("{ctx}.entities.{}", old_e.name),
                description: format!("Removed entity '{}' from context '{}'", old_e.name, ctx),
                before: Some(serde_json::to_value(old_e).unwrap()),
                after: None,
            });
        }
    }

    // Services
    for new_s in &new.services {
        match old
            .services
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(&new_s.name))
        {
            None => {
                changes.push(ModelChange {
                    kind: ChangeKind::Added,
                    path: format!("{ctx}.services.{}", new_s.name),
                    description: format!("New service '{}' in context '{}'", new_s.name, ctx),
                    before: None,
                    after: Some(serde_json::to_value(new_s).unwrap()),
                });
            }
            Some(old_s) => {
                diff_service(ctx, old_s, new_s, changes);
            }
        }
    }
    for old_s in &old.services {
        if !new
            .services
            .iter()
            .any(|s| s.name.eq_ignore_ascii_case(&old_s.name))
        {
            changes.push(ModelChange {
                kind: ChangeKind::Removed,
                path: format!("{ctx}.services.{}", old_s.name),
                description: format!("Removed service '{}' from context '{}'", old_s.name, ctx),
                before: Some(serde_json::to_value(old_s).unwrap()),
                after: None,
            });
        }
    }

    // Events
    for new_ev in &new.events {
        if !old
            .events
            .iter()
            .any(|e| e.name.eq_ignore_ascii_case(&new_ev.name))
        {
            changes.push(ModelChange {
                kind: ChangeKind::Added,
                path: format!("{ctx}.events.{}", new_ev.name),
                description: format!("New event '{}' in context '{}'", new_ev.name, ctx),
                before: None,
                after: Some(serde_json::to_value(new_ev).unwrap()),
            });
        }
    }
    for old_ev in &old.events {
        if !new
            .events
            .iter()
            .any(|e| e.name.eq_ignore_ascii_case(&old_ev.name))
        {
            changes.push(ModelChange {
                kind: ChangeKind::Removed,
                path: format!("{ctx}.events.{}", old_ev.name),
                description: format!("Removed event '{}' from context '{}'", old_ev.name, ctx),
                before: Some(serde_json::to_value(old_ev).unwrap()),
                after: None,
            });
        }
    }

    // Value objects
    for new_vo in &new.value_objects {
        if !old
            .value_objects
            .iter()
            .any(|v| v.name.eq_ignore_ascii_case(&new_vo.name))
        {
            changes.push(ModelChange {
                kind: ChangeKind::Added,
                path: format!("{ctx}.value_objects.{}", new_vo.name),
                description: format!("New value object '{}' in context '{}'", new_vo.name, ctx),
                before: None,
                after: Some(serde_json::to_value(new_vo).unwrap()),
            });
        }
    }
    for old_vo in &old.value_objects {
        if !new
            .value_objects
            .iter()
            .any(|v| v.name.eq_ignore_ascii_case(&old_vo.name))
        {
            changes.push(ModelChange {
                kind: ChangeKind::Removed,
                path: format!("{ctx}.value_objects.{}", old_vo.name),
                description: format!("Removed value object '{}' from context '{}'", old_vo.name, ctx),
                before: Some(serde_json::to_value(old_vo).unwrap()),
                after: None,
            });
        }
    }

    // Repositories
    for new_r in &new.repositories {
        if !old
            .repositories
            .iter()
            .any(|r| r.name.eq_ignore_ascii_case(&new_r.name))
        {
            changes.push(ModelChange {
                kind: ChangeKind::Added,
                path: format!("{ctx}.repositories.{}", new_r.name),
                description: format!("New repository '{}' in context '{}'", new_r.name, ctx),
                before: None,
                after: Some(serde_json::to_value(new_r).unwrap()),
            });
        }
    }
    for old_r in &old.repositories {
        if !new
            .repositories
            .iter()
            .any(|r| r.name.eq_ignore_ascii_case(&old_r.name))
        {
            changes.push(ModelChange {
                kind: ChangeKind::Removed,
                path: format!("{ctx}.repositories.{}", old_r.name),
                description: format!("Removed repository '{}' from context '{}'", old_r.name, ctx),
                before: Some(serde_json::to_value(old_r).unwrap()),
                after: None,
            });
        }
    }

    // Dependency changes
    for dep in &new.dependencies {
        if !old
            .dependencies
            .iter()
            .any(|d| d.eq_ignore_ascii_case(dep))
        {
            changes.push(ModelChange {
                kind: ChangeKind::Added,
                path: format!("{ctx}.dependencies.{dep}"),
                description: format!("New dependency: {} → {}", ctx, dep),
                before: None,
                after: Some(json!(dep)),
            });
        }
    }
    for dep in &old.dependencies {
        if !new
            .dependencies
            .iter()
            .any(|d| d.eq_ignore_ascii_case(dep))
        {
            changes.push(ModelChange {
                kind: ChangeKind::Removed,
                path: format!("{ctx}.dependencies.{dep}"),
                description: format!("Removed dependency: {} → {}", ctx, dep),
                before: Some(json!(dep)),
                after: None,
            });
        }
    }
}

fn diff_entity(
    ctx: &str,
    old: &Entity,
    new: &Entity,
    changes: &mut Vec<ModelChange>,
) {
    let name = &new.name;

    // Aggregate root change
    if old.aggregate_root != new.aggregate_root {
        changes.push(ModelChange {
            kind: ChangeKind::Modified,
            path: format!("{ctx}.{name}.aggregate_root"),
            description: format!(
                "'{}' aggregate root: {} → {}",
                name, old.aggregate_root, new.aggregate_root
            ),
            before: Some(json!(old.aggregate_root)),
            after: Some(json!(new.aggregate_root)),
        });
    }

    // Fields added/removed
    for new_f in &new.fields {
        if !old
            .fields
            .iter()
            .any(|f| f.name.eq_ignore_ascii_case(&new_f.name))
        {
            changes.push(ModelChange {
                kind: ChangeKind::Added,
                path: format!("{ctx}.{name}.fields.{}", new_f.name),
                description: format!(
                    "New field '{}: {}' on entity '{}'",
                    new_f.name, new_f.field_type, name
                ),
                before: None,
                after: Some(serde_json::to_value(new_f).unwrap()),
            });
        }
    }
    for old_f in &old.fields {
        if !new
            .fields
            .iter()
            .any(|f| f.name.eq_ignore_ascii_case(&old_f.name))
        {
            changes.push(ModelChange {
                kind: ChangeKind::Removed,
                path: format!("{ctx}.{name}.fields.{}", old_f.name),
                description: format!("Removed field '{}' from entity '{}'", old_f.name, name),
                before: Some(serde_json::to_value(old_f).unwrap()),
                after: None,
            });
        }
    }

    // Field type changes
    for new_f in &new.fields {
        if let Some(old_f) = old
            .fields
            .iter()
            .find(|f| f.name.eq_ignore_ascii_case(&new_f.name))
        {
            if old_f.field_type != new_f.field_type {
                changes.push(ModelChange {
                    kind: ChangeKind::Modified,
                    path: format!("{ctx}.{name}.fields.{}", new_f.name),
                    description: format!(
                        "Field '{}' on '{}' type changed: {} → {}",
                        new_f.name, name, old_f.field_type, new_f.field_type
                    ),
                    before: Some(json!(old_f.field_type)),
                    after: Some(json!(new_f.field_type)),
                });
            }
        }
    }

    // Invariant changes
    for inv in &new.invariants {
        if !old.invariants.iter().any(|i| i == inv) {
            changes.push(ModelChange {
                kind: ChangeKind::Added,
                path: format!("{ctx}.{name}.invariants"),
                description: format!("New invariant on '{}': {}", name, inv),
                before: None,
                after: Some(json!(inv)),
            });
        }
    }
}

fn diff_service(
    ctx: &str,
    old: &Service,
    new: &Service,
    changes: &mut Vec<ModelChange>,
) {
    let name = &new.name;

    // Kind change
    let old_kind = format!("{:?}", old.kind);
    let new_kind = format!("{:?}", new.kind);
    if old_kind != new_kind {
        changes.push(ModelChange {
            kind: ChangeKind::Modified,
            path: format!("{ctx}.services.{name}.kind"),
            description: format!(
                "Service '{}' kind changed: {} → {}",
                name, old_kind, new_kind
            ),
            before: Some(json!(old_kind)),
            after: Some(json!(new_kind)),
        });
    }

    // Methods added/removed
    for new_m in &new.methods {
        if !old.methods.iter().any(|m| m.name.eq_ignore_ascii_case(&new_m.name)) {
            changes.push(ModelChange {
                kind: ChangeKind::Added,
                path: format!("{ctx}.services.{name}.methods.{}", new_m.name),
                description: format!("New method '{}' on service '{}'", new_m.name, name),
                before: None,
                after: Some(serde_json::to_value(new_m).unwrap()),
            });
        }
    }
    for old_m in &old.methods {
        if !new.methods.iter().any(|m| m.name.eq_ignore_ascii_case(&old_m.name)) {
            changes.push(ModelChange {
                kind: ChangeKind::Removed,
                path: format!("{ctx}.services.{name}.methods.{}", old_m.name),
                description: format!("Removed method '{}' from service '{}'", old_m.name, name),
                before: Some(serde_json::to_value(old_m).unwrap()),
                after: None,
            });
        }
    }

    // Dependency changes
    for dep in &new.dependencies {
        if !old.dependencies.iter().any(|d| d.eq_ignore_ascii_case(dep)) {
            changes.push(ModelChange {
                kind: ChangeKind::Added,
                path: format!("{ctx}.services.{name}.dependencies.{dep}"),
                description: format!("New dependency on service '{}': {}", name, dep),
                before: None,
                after: Some(json!(dep)),
            });
        }
    }
    for dep in &old.dependencies {
        if !new.dependencies.iter().any(|d| d.eq_ignore_ascii_case(dep)) {
            changes.push(ModelChange {
                kind: ChangeKind::Removed,
                path: format!("{ctx}.services.{name}.dependencies.{dep}"),
                description: format!("Removed dependency on service '{}': {}", name, dep),
                before: Some(json!(dep)),
                after: None,
            });
        }
    }
}

/// Generate a refactoring plan from model changes.
pub fn plan_refactoring(
    changes: &[ModelChange],
    conventions: &Conventions,
) -> RefactoringPlan {
    let mut code_actions = Vec::new();
    let mut migration_notes = Vec::new();
    let pattern = &conventions.file_structure.pattern;

    for change in changes {
        match &change.kind {
            ChangeKind::Added => {
                let parts: Vec<&str> = change.path.split('.').collect();
                match parts.as_slice() {
                    // New bounded context
                    [_bc_key, ctx_name] if change.path.starts_with("bounded_contexts.") => {
                        let ctx_snake = to_snake(ctx_name);
                        for layer in &conventions.file_structure.layers {
                            code_actions.push(CodeAction {
                                action: ActionKind::CreateFile,
                                file_path: format!("src/{ctx_snake}/{layer}/mod.rs"),
                                description: format!("Create {layer} layer module for context '{ctx_name}'"),
                                priority: Priority::High,
                            });
                        }
                    }
                    // New entity
                    [ctx, _, entity_name] if change.path.contains(".entities.") => {
                        let file = resolve_path(pattern, ctx, "domain", entity_name);
                        code_actions.push(CodeAction {
                            action: ActionKind::CreateFile,
                            file_path: file,
                            description: format!("Create entity '{entity_name}'"),
                            priority: Priority::High,
                        });
                        code_actions.push(CodeAction {
                            action: ActionKind::AddTest,
                            file_path: resolve_path(pattern, ctx, "domain", entity_name),
                            description: format!("Add unit tests for entity '{entity_name}'"),
                            priority: Priority::Medium,
                        });
                        migration_notes.push(format!(
                            "New entity '{}' — may need database migration",
                            entity_name
                        ));
                    }
                    // New field on entity
                    [ctx, entity, _, field_name] if change.path.contains(".fields.") => {
                        let file = resolve_path(pattern, ctx, "domain", entity);
                        code_actions.push(CodeAction {
                            action: ActionKind::ModifyFile,
                            file_path: file,
                            description: format!("Add field '{field_name}' to entity '{entity}'"),
                            priority: Priority::High,
                        });
                        migration_notes.push(format!(
                            "New field '{field_name}' on '{entity}' — needs ALTER TABLE migration"
                        ));
                    }
                    // New service
                    [ctx, _, svc_name] if change.path.contains(".services.") => {
                        let file = resolve_path(pattern, ctx, "application", svc_name);
                        code_actions.push(CodeAction {
                            action: ActionKind::CreateFile,
                            file_path: file,
                            description: format!("Create service '{svc_name}'"),
                            priority: Priority::High,
                        });
                    }
                    // New event
                    [ctx, _, event_name] if change.path.contains(".events.") => {
                        let file = resolve_path(pattern, ctx, "domain", event_name);
                        code_actions.push(CodeAction {
                            action: ActionKind::CreateFile,
                            file_path: file,
                            description: format!("Create domain event '{event_name}'"),
                            priority: Priority::Medium,
                        });
                    }
                    // New invariant
                    [ctx, entity, _] if change.path.contains(".invariants") => {
                        code_actions.push(CodeAction {
                            action: ActionKind::AddTest,
                            file_path: resolve_path(pattern, ctx, "domain", entity),
                            description: format!("Add test for new invariant on '{entity}'"),
                            priority: Priority::Medium,
                        });
                    }
                    // New dependency
                    [ctx, _, target] if change.path.contains(".dependencies.") => {
                        code_actions.push(CodeAction {
                            action: ActionKind::UpdateImports,
                            file_path: format!("src/{}/mod.rs", to_snake(ctx)),
                            description: format!("Wire dependency '{ctx}' → '{target}'"),
                            priority: Priority::Medium,
                        });
                    }
                    _ => {}
                }
            }
            ChangeKind::Removed => {
                let parts: Vec<&str> = change.path.split('.').collect();
                match parts.as_slice() {
                    [ctx, _, entity_name] if change.path.contains(".entities.") => {
                        let file = resolve_path(pattern, ctx, "domain", entity_name);
                        code_actions.push(CodeAction {
                            action: ActionKind::DeleteFile,
                            file_path: file,
                            description: format!("Remove entity '{entity_name}' and all references"),
                            priority: Priority::Critical,
                        });
                        migration_notes.push(format!(
                            "Removed entity '{}' — needs DROP TABLE migration",
                            entity_name
                        ));
                    }
                    [ctx, entity, _, field_name] if change.path.contains(".fields.") => {
                        let file = resolve_path(pattern, ctx, "domain", entity);
                        code_actions.push(CodeAction {
                            action: ActionKind::ModifyFile,
                            file_path: file,
                            description: format!(
                                "Remove field '{field_name}' from entity '{entity}'"
                            ),
                            priority: Priority::High,
                        });
                        migration_notes.push(format!(
                            "Removed field '{field_name}' from '{entity}' — needs ALTER TABLE migration"
                        ));
                    }
                    _ => {}
                }
            }
            ChangeKind::Modified => {
                let parts: Vec<&str> = change.path.split('.').collect();
                match parts.as_slice() {
                    [ctx, entity, _, field_name] if change.path.contains(".fields.") => {
                        let file = resolve_path(pattern, ctx, "domain", entity);
                        code_actions.push(CodeAction {
                            action: ActionKind::ModifyFile,
                            file_path: file,
                            description: format!(
                                "Update field type for '{field_name}' on '{entity}'"
                            ),
                            priority: Priority::Critical,
                        });
                        migration_notes.push(format!(
                            "Field type change on '{entity}.{field_name}' — needs data migration"
                        ));
                    }
                    _ => {}
                }
            }
            ChangeKind::Moved => {
                if change.path.contains("module_path") {
                    if let (Some(from), Some(to)) = (&change.before, &change.after) {
                        code_actions.push(CodeAction {
                            action: ActionKind::MoveFile,
                            file_path: from.as_str().unwrap_or("").to_string(),
                            description: format!(
                                "Move module from {} to {}",
                                from.as_str().unwrap_or("?"),
                                to.as_str().unwrap_or("?")
                            ),
                            priority: Priority::Critical,
                        });
                    }
                }
            }
        }
    }

    // Sort by priority
    code_actions.sort_by_key(|a| match a.priority {
        Priority::Critical => 0,
        Priority::High => 1,
        Priority::Medium => 2,
        Priority::Low => 3,
    });

    RefactoringPlan {
        model_changes: changes.to_vec(),
        code_actions,
        migration_notes,
    }
}

fn resolve_path(pattern: &str, context: &str, layer: &str, name: &str) -> String {
    if pattern.is_empty() {
        return format!("src/{}/{}/{}.rs", to_snake(context), layer, to_snake(name));
    }
    pattern
        .replace("{context}", &to_snake(context))
        .replace("{layer}", layer)
        .replace("{type}", &to_snake(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_model() -> DomainModel {
        DomainModel {
            name: "Test".into(),
            description: "".into(),
            bounded_contexts: vec![BoundedContext {
                name: "Identity".into(),
                description: "".into(),
                module_path: "src/identity".into(),
                entities: vec![Entity {
                    name: "User".into(),
                    description: "".into(),
                    aggregate_root: true,
                    fields: vec![Field {
                        name: "id".into(),
                        field_type: "UserId".into(),
                        required: true,
                        description: "".into(),
                    }],
                    methods: vec![],
                    invariants: vec![],
                }],
                value_objects: vec![],
                services: vec![],
                repositories: vec![],
                events: vec![],
                dependencies: vec![],
            }],
            rules: vec![],
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
    fn test_no_changes_for_identical_models() {
        let m = base_model();
        let changes = diff_models(&m, &m);
        assert!(changes.is_empty());
    }

    #[test]
    fn test_detect_new_entity() {
        let old = base_model();
        let mut new = base_model();
        new.bounded_contexts[0].entities.push(Entity {
            name: "Role".into(),
            description: "".into(),
            aggregate_root: false,
            fields: vec![],
            methods: vec![],
            invariants: vec![],
        });
        let changes = diff_models(&old, &new);
        assert_eq!(changes.len(), 1);
        assert!(matches!(changes[0].kind, ChangeKind::Added));
        assert!(changes[0].path.contains("Role"));
    }

    #[test]
    fn test_detect_removed_entity() {
        let old = base_model();
        let mut new = base_model();
        new.bounded_contexts[0].entities.clear();
        let changes = diff_models(&old, &new);
        assert_eq!(changes.len(), 1);
        assert!(matches!(changes[0].kind, ChangeKind::Removed));
    }

    #[test]
    fn test_detect_new_field() {
        let old = base_model();
        let mut new = base_model();
        new.bounded_contexts[0].entities[0].fields.push(Field {
            name: "email".into(),
            field_type: "String".into(),
            required: true,
            description: "".into(),
        });
        let changes = diff_models(&old, &new);
        assert_eq!(changes.len(), 1);
        assert!(changes[0].path.contains("email"));
    }

    #[test]
    fn test_detect_field_type_change() {
        let old = base_model();
        let mut new = base_model();
        new.bounded_contexts[0].entities[0].fields[0].field_type = "Uuid".into();
        let changes = diff_models(&old, &new);
        assert_eq!(changes.len(), 1);
        assert!(matches!(changes[0].kind, ChangeKind::Modified));
    }

    #[test]
    fn test_detect_new_bounded_context() {
        let old = base_model();
        let mut new = base_model();
        new.bounded_contexts.push(BoundedContext {
            name: "Billing".into(),
            description: "".into(),
            module_path: "src/billing".into(),
            entities: vec![],
            value_objects: vec![],
            services: vec![],
            repositories: vec![],
            events: vec![],
            dependencies: vec!["Identity".into()],
        });
        let changes = diff_models(&old, &new);
        // New context + new dependency
        assert!(changes.iter().any(|c| matches!(c.kind, ChangeKind::Added)
            && c.path.contains("Billing")));
    }

    #[test]
    fn test_detect_module_move() {
        let old = base_model();
        let mut new = base_model();
        new.bounded_contexts[0].module_path = "src/auth".into();
        let changes = diff_models(&old, &new);
        assert_eq!(changes.len(), 1);
        assert!(matches!(changes[0].kind, ChangeKind::Moved));
    }

    #[test]
    fn test_detect_removed_event() {
        let old = {
            let mut m = base_model();
            m.bounded_contexts[0].events.push(DomainEvent {
                name: "UserCreated".into(),
                description: "".into(),
                fields: vec![],
                source: "User".into(),
            });
            m
        };
        let new = base_model();
        let changes = diff_models(&old, &new);
        assert!(changes.iter().any(|c| matches!(c.kind, ChangeKind::Removed)
            && c.path.contains("UserCreated")));
    }

    #[test]
    fn test_detect_service_kind_change() {
        let old = {
            let mut m = base_model();
            m.bounded_contexts[0].services.push(Service {
                name: "AuthService".into(),
                description: "".into(),
                kind: ServiceKind::Domain,
                methods: vec![],
                dependencies: vec![],
            });
            m
        };
        let mut new = old.clone();
        new.bounded_contexts[0].services[0].kind = ServiceKind::Application;
        let changes = diff_models(&old, &new);
        assert!(changes.iter().any(|c| matches!(c.kind, ChangeKind::Modified)
            && c.path.contains("AuthService")));
    }

    #[test]
    fn test_detect_new_value_object() {
        let old = base_model();
        let mut new = base_model();
        new.bounded_contexts[0].value_objects.push(ValueObject {
            name: "Email".into(),
            description: "".into(),
            fields: vec![],
            validation_rules: vec![],
        });
        let changes = diff_models(&old, &new);
        assert!(changes.iter().any(|c| matches!(c.kind, ChangeKind::Added)
            && c.path.contains("Email")));
    }

    #[test]
    fn test_detect_new_repository() {
        let old = base_model();
        let mut new = base_model();
        new.bounded_contexts[0].repositories.push(Repository {
            name: "UserRepository".into(),
            aggregate: "User".into(),
            methods: vec![],
        });
        let changes = diff_models(&old, &new);
        assert!(changes.iter().any(|c| matches!(c.kind, ChangeKind::Added)
            && c.path.contains("UserRepository")));
    }

    #[test]
    fn test_detect_modified_rule() {
        let old = {
            let mut m = base_model();
            m.rules.push(ArchitecturalRule {
                id: "RULE-1".into(),
                description: "Old description".into(),
                severity: Severity::Warning,
                scope: "".into(),
            });
            m
        };
        let mut new = old.clone();
        new.rules[0].description = "New description".into();
        let changes = diff_models(&old, &new);
        assert!(changes.iter().any(|c| matches!(c.kind, ChangeKind::Modified)
            && c.path.contains("RULE-1")));
    }

    #[test]
    fn test_plan_refactoring_creates_file_for_new_entity() {
        let old = base_model();
        let mut new = base_model();
        new.bounded_contexts[0].entities.push(Entity {
            name: "Role".into(),
            description: "".into(),
            aggregate_root: false,
            fields: vec![],
            methods: vec![],
            invariants: vec![],
        });
        let changes = diff_models(&old, &new);
        let plan = plan_refactoring(&changes, &new.conventions);
        assert!(!plan.code_actions.is_empty());
        assert!(plan.code_actions.iter().any(|a| matches!(a.action, ActionKind::CreateFile)
            && a.file_path.contains("role")));
    }

    #[test]
    fn test_plan_refactoring_field_migration_note() {
        let old = base_model();
        let mut new = base_model();
        new.bounded_contexts[0].entities[0].fields.push(Field {
            name: "avatar".into(),
            field_type: "String".into(),
            required: false,
            description: "".into(),
        });
        let changes = diff_models(&old, &new);
        let plan = plan_refactoring(&changes, &new.conventions);
        assert!(plan.migration_notes.iter().any(|n| n.contains("ALTER TABLE")));
    }
}
