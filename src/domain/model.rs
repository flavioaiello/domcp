use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

// ─── Top-Level Domain Model ────────────────────────────────────────────────

/// The root of the domain model configuration.
/// Describes the entire system architecture that Copilot should adhere to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainModel {
    /// Human-readable project name
    pub name: String,
    /// Project description
    #[serde(default)]
    pub description: String,
    /// Bounded contexts (DDD)
    #[serde(default)]
    pub bounded_contexts: Vec<BoundedContext>,
    /// Cross-cutting architectural rules
    #[serde(default)]
    pub rules: Vec<ArchitecturalRule>,
    /// Technology stack constraints
    #[serde(default)]
    pub tech_stack: TechStack,
    /// Naming conventions
    #[serde(default)]
    pub conventions: Conventions,
}

impl DomainModel {
    /// Create an empty model for a new workspace.
    pub fn empty(workspace_path: &str) -> Self {
        let name = std::path::Path::new(workspace_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "Unnamed".into());
        Self {
            name,
            description: String::new(),
            bounded_contexts: vec![],
            rules: vec![],
            tech_stack: TechStack::default(),
            conventions: Conventions::default(),
        }
    }

    /// Load from a JSON file (used by import).
    pub fn load(path: &str) -> Result<Self> {
        let path = Path::new(path);
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read domain model from {}", path.display()))?;
        let model: DomainModel = serde_json::from_str(&content)
            .with_context(|| "Failed to parse domain model JSON")?;
        model.validate()?;
        Ok(model)
    }

    fn validate(&self) -> Result<()> {
        if self.name.is_empty() {
            anyhow::bail!("Domain model must have a name");
        }
        for bc in &self.bounded_contexts {
            if bc.name.is_empty() {
                anyhow::bail!("Bounded context must have a name");
            }
            for entity in &bc.entities {
                if entity.name.is_empty() {
                    anyhow::bail!(
                        "Entity in bounded context '{}' must have a name",
                        bc.name
                    );
                }
            }
        }
        Ok(())
    }
}

// ─── Bounded Context ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoundedContext {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Module path / namespace for this context
    #[serde(default)]
    pub module_path: String,
    #[serde(default)]
    pub entities: Vec<Entity>,
    #[serde(default)]
    pub value_objects: Vec<ValueObject>,
    #[serde(default)]
    pub services: Vec<Service>,
    #[serde(default)]
    pub repositories: Vec<Repository>,
    #[serde(default)]
    pub events: Vec<DomainEvent>,
    /// Allowed dependencies to other bounded contexts
    #[serde(default)]
    pub dependencies: Vec<String>,
}

// ─── Entity ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Whether this is an aggregate root
    #[serde(default)]
    pub aggregate_root: bool,
    #[serde(default)]
    pub fields: Vec<Field>,
    #[serde(default)]
    pub methods: Vec<Method>,
    #[serde(default)]
    pub invariants: Vec<String>,
}

// ─── Value Object ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValueObject {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub fields: Vec<Field>,
    #[serde(default)]
    pub validation_rules: Vec<String>,
}

// ─── Service ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Service {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub kind: ServiceKind,
    #[serde(default)]
    pub methods: Vec<Method>,
    #[serde(default)]
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceKind {
    #[default]
    Domain,
    Application,
    Infrastructure,
}

// ─── Repository ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repository {
    pub name: String,
    /// The aggregate root this repository manages
    pub aggregate: String,
    #[serde(default)]
    pub methods: Vec<Method>,
}

// ─── Domain Event ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainEvent {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub fields: Vec<Field>,
    /// Which entity/aggregate emits this event
    #[serde(default)]
    pub source: String,
}

// ─── Shared Building Blocks ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Field {
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Method {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub parameters: Vec<Field>,
    #[serde(default)]
    pub return_type: String,
}

// ─── Architectural Rules ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchitecturalRule {
    pub id: String,
    pub description: String,
    #[serde(default)]
    pub severity: Severity,
    /// The pattern/layer this rule applies to
    #[serde(default)]
    pub scope: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    #[default]
    Error,
    Warning,
    Info,
}

// ─── Tech Stack ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TechStack {
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub framework: String,
    #[serde(default)]
    pub database: String,
    #[serde(default)]
    pub messaging: String,
    #[serde(default)]
    pub additional: Vec<String>,
}

// ─── Conventions ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Conventions {
    #[serde(default)]
    pub naming: NamingConventions,
    #[serde(default)]
    pub file_structure: FileStructure,
    #[serde(default)]
    pub error_handling: String,
    #[serde(default)]
    pub testing: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NamingConventions {
    #[serde(default)]
    pub entities: String,
    #[serde(default)]
    pub value_objects: String,
    #[serde(default)]
    pub services: String,
    #[serde(default)]
    pub repositories: String,
    #[serde(default)]
    pub events: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileStructure {
    /// e.g. "src/{context}/{layer}/{type}.rs"
    #[serde(default)]
    pub pattern: String,
    #[serde(default)]
    pub layers: Vec<String>,
}
