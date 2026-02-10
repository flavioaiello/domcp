use crate::domain::model::DomainModel;
use crate::mcp::protocol::*;

/// Returns the list of prompts the DOMCP server exposes.
pub fn list_prompts() -> Vec<PromptDefinition> {
    vec![PromptDefinition {
        name: "domcp_guidelines".into(),
        description: "Architecture guidelines and mandatory tool usage for DOMCP. \
                      Use this prompt to understand how to work with the domain model \
                      and which tools to call before writing or modifying code."
            .into(),
        arguments: vec![],
    }]
}

/// Resolve a prompt by name.
pub fn get_prompt(model: &DomainModel, name: &str) -> Option<PromptGetResult> {
    match name {
        "domcp_guidelines" => Some(build_guidelines_prompt(model)),
        _ => None,
    }
}

fn build_guidelines_prompt(model: &DomainModel) -> PromptGetResult {
    let project_name = &model.name;
    let is_empty = model.bounded_contexts.is_empty();

    let context_line = if is_empty {
        "No bounded contexts defined yet.".to_string()
    } else {
        let names: Vec<&str> = model
            .bounded_contexts
            .iter()
            .map(|bc| bc.name.as_str())
            .collect();
        format!("Bounded contexts: {}", names.join(", "))
    };

    let bootstrap = if is_empty {
        "\n**This project has no domain model yet.** \
         Analyze the codebase first: identify bounded contexts, entities, services, \
         and events using the write tools, then call `save_model` to persist.\n"
    } else {
        ""
    };

    let rules_section = if model.rules.is_empty() {
        String::new()
    } else {
        let rules: Vec<String> = model
            .rules
            .iter()
            .map(|r| format!("- **{}** ({}): {}", r.id, format!("{:?}", r.severity).to_lowercase(), r.description))
            .collect();
        format!("\n### Rules\n\n{}\n", rules.join("\n"))
    };

    let text = format!(
        r#"## DOMCP — {project_name}

{context_line}
{bootstrap}
### Workflow

1. **Before writing code** → call `get_architecture_overview`
2. **Before creating files** → call `suggest_file_path`
3. **Before cross-context imports** → call `validate_dependency`
4. **After model changes** → call `compare_model`, then `draft_refactoring_plan`, then `save_model`
{rules_section}"#
    );

    PromptGetResult {
        description: format!("Architecture guidelines for {project_name}"),
        messages: vec![PromptMessage {
            role: "user".into(),
            content: ContentBlock::Text { text },
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
                description: "".into(),
                module_path: "".into(),
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
    fn test_list_prompts() {
        let prompts = list_prompts();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].name, "domcp_guidelines");
    }

    #[test]
    fn test_get_prompt_found() {
        let model = test_model();
        let result = get_prompt(&model, "domcp_guidelines");
        assert!(result.is_some());
        let prompt = result.unwrap();
        assert!(prompt.description.contains("TestProject"));
        assert_eq!(prompt.messages.len(), 1);
    }

    #[test]
    fn test_get_prompt_not_found() {
        let model = test_model();
        assert!(get_prompt(&model, "nonexistent").is_none());
    }

    #[test]
    fn test_prompt_includes_contexts() {
        let model = test_model();
        let prompt = get_prompt(&model, "domcp_guidelines").unwrap();
        let text = match &prompt.messages[0].content {
            ContentBlock::Text { text } => text,
        };
        assert!(text.contains("Identity"));
    }
}
