use crate::domain::model::DomainModel;
use crate::domain::registry::DomainRegistry;
use crate::mcp::protocol::*;

/// Returns the list of resources the DOMCP server exposes.
pub fn list_resources(model: &DomainModel) -> Vec<ResourceDefinition> {
    let mut resources = vec![
        ResourceDefinition {
            uri: "domcp://architecture/overview".into(),
            name: "Architecture Overview".into(),
            description: "Complete architecture overview with all bounded contexts, entities, and rules".into(),
            mime_type: "application/json".into(),
        },
        ResourceDefinition {
            uri: "domcp://architecture/rules".into(),
            name: "Architectural Rules".into(),
            description: "All architectural constraints and rules".into(),
            mime_type: "application/json".into(),
        },
        ResourceDefinition {
            uri: "domcp://architecture/conventions".into(),
            name: "Conventions".into(),
            description: "Naming, file structure, error handling, and testing conventions".into(),
            mime_type: "application/json".into(),
        },
    ];

    // Add per-context resources
    for bc in &model.bounded_contexts {
        resources.push(ResourceDefinition {
            uri: format!("domcp://context/{}", bc.name.to_lowercase()),
            name: format!("Context: {}", bc.name),
            description: format!(
                "Bounded context '{}' — entities, services, events",
                bc.name
            ),
            mime_type: "application/json".into(),
        });
    }

    resources
}

/// Reads a resource by URI.
pub fn read_resource(model: &DomainModel, uri: &str) -> ResourceReadResult {
    let registry = DomainRegistry::new(model);

    let (mime, text) = match uri {
        "domcp://architecture/overview" => ("application/json", registry.architecture_summary()),
        "domcp://architecture/rules" => (
            "application/json",
            serde_json::to_string(&model.rules).unwrap_or_default(),
        ),
        "domcp://architecture/conventions" => (
            "application/json",
            serde_json::to_string(&model.conventions).unwrap_or_default(),
        ),
        _ if uri.starts_with("domcp://context/") => {
            let ctx_name = uri.strip_prefix("domcp://context/").unwrap_or("");
            match registry.find_context(ctx_name) {
                Some(bc) => (
                    "application/json",
                    serde_json::to_string(bc).unwrap_or_default(),
                ),
                None => (
                    "text/plain",
                    format!("Bounded context '{}' not found", ctx_name),
                ),
            }
        }
        _ => ("text/plain", format!("Unknown resource: {}", uri)),
    };

    ResourceReadResult {
        contents: vec![ResourceContent {
            uri: uri.to_string(),
            mime_type: mime.to_string(),
            text,
        }],
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
            bounded_contexts: vec![BoundedContext {
                name: "Identity".into(),
                description: "Auth context".into(),
                module_path: "src/identity".into(),
                entities: vec![],
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
    fn test_list_resources_includes_static_and_context() {
        let model = test_model();
        let resources = list_resources(&model);
        // 3 static + 1 per context
        assert_eq!(resources.len(), 4);
        assert!(resources.iter().any(|r| r.uri == "domcp://architecture/overview"));
        assert!(resources.iter().any(|r| r.uri == "domcp://context/identity"));
    }

    #[test]
    fn test_read_resource_overview() {
        let model = test_model();
        let result = read_resource(&model, "domcp://architecture/overview");
        assert_eq!(result.contents.len(), 1);
        assert_eq!(result.contents[0].mime_type, "application/json");
        assert!(result.contents[0].text.contains("TestProject"));
    }

    #[test]
    fn test_read_resource_context() {
        let model = test_model();
        let result = read_resource(&model, "domcp://context/identity");
        assert!(result.contents[0].text.contains("Identity"));
    }

    #[test]
    fn test_read_resource_unknown() {
        let model = test_model();
        let result = read_resource(&model, "domcp://unknown");
        assert!(result.contents[0].text.contains("Unknown resource"));
    }

    #[test]
    fn test_read_resource_context_not_found() {
        let model = test_model();
        let result = read_resource(&model, "domcp://context/nonexistent");
        assert!(result.contents[0].text.contains("not found"));
    }
}
