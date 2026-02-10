use super::model::*;

/// Provides query access into the domain model for MCP tool handlers.
pub struct DomainRegistry<'a> {
    model: &'a DomainModel,
}

impl<'a> DomainRegistry<'a> {
    pub fn new(model: &'a DomainModel) -> Self {
        Self { model }
    }

    pub fn find_context(&self, name: &str) -> Option<&BoundedContext> {
        self.model
            .bounded_contexts
            .iter()
            .find(|bc| bc.name.eq_ignore_ascii_case(name))
    }

    pub fn find_entity(&self, name: &str) -> Option<(&BoundedContext, &Entity)> {
        for bc in &self.model.bounded_contexts {
            if let Some(entity) = bc.entities.iter().find(|e| e.name.eq_ignore_ascii_case(name)) {
                return Some((bc, entity));
            }
        }
        None
    }

    pub fn find_service(&self, name: &str) -> Option<(&BoundedContext, &Service)> {
        for bc in &self.model.bounded_contexts {
            if let Some(svc) = bc.services.iter().find(|s| s.name.eq_ignore_ascii_case(name)) {
                return Some((bc, svc));
            }
        }
        None
    }

    pub fn context_names(&self) -> Vec<&str> {
        self.model
            .bounded_contexts
            .iter()
            .map(|bc| bc.name.as_str())
            .collect()
    }

    /// Produce a structured JSON summary for Copilot context injection.
    /// Compact and machine-readable — no prose, just data.
    pub fn architecture_summary(&self) -> String {
        use serde_json::json;

        let contexts: Vec<_> = self.model.bounded_contexts.iter().map(|bc| {
            json!({
                "name": bc.name,
                "module": bc.module_path,
                "entities": bc.entities.iter().map(|e| {
                    json!({
                        "name": e.name,
                        "aggregate_root": e.aggregate_root,
                        "fields": e.fields.iter().map(|f| {
                            format!("{}: {}{}", f.name, f.field_type, if f.required { " (required)" } else { "" })
                        }).collect::<Vec<_>>(),
                        "methods": e.methods.iter().map(|m| {
                            format!("{}({}) -> {}", m.name,
                                m.parameters.iter().map(|p| format!("{}: {}", p.name, p.field_type)).collect::<Vec<_>>().join(", "),
                                m.return_type)
                        }).collect::<Vec<_>>(),
                        "invariants": e.invariants,
                    })
                }).collect::<Vec<_>>(),
                "value_objects": bc.value_objects.iter().map(|v| &v.name).collect::<Vec<_>>(),
                "services": bc.services.iter().map(|s| {
                    json!({ "name": s.name, "kind": format!("{:?}", s.kind) })
                }).collect::<Vec<_>>(),
                "events": bc.events.iter().map(|e| &e.name).collect::<Vec<_>>(),
                "repositories": bc.repositories.iter().map(|r| {
                    json!({ "name": r.name, "aggregate": r.aggregate })
                }).collect::<Vec<_>>(),
                "depends_on": bc.dependencies,
            })
        }).collect();

        let rules: Vec<_> = self.model.rules.iter().map(|r| {
            json!({ "id": r.id, "severity": format!("{:?}", r.severity), "rule": r.description })
        }).collect();

        let overview = json!({
            "project": self.model.name,
            "tech": {
                "language": self.model.tech_stack.language,
                "framework": self.model.tech_stack.framework,
                "database": self.model.tech_stack.database,
                "messaging": self.model.tech_stack.messaging,
            },
            "bounded_contexts": contexts,
            "rules": rules,
            "conventions": {
                "file_pattern": self.model.conventions.file_structure.pattern,
                "layers": self.model.conventions.file_structure.layers,
                "naming": {
                    "entities": self.model.conventions.naming.entities,
                    "services": self.model.conventions.naming.services,
                    "events": self.model.conventions.naming.events,
                    "value_objects": self.model.conventions.naming.value_objects,
                    "repositories": self.model.conventions.naming.repositories,
                },
                "error_handling": self.model.conventions.error_handling,
                "testing": self.model.conventions.testing,
            }
        });

        serde_json::to_string(&overview).unwrap()
    }
}
