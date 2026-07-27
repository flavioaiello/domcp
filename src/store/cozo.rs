use anyhow::{Context, Result};
use cozo::{DbInstance, ScriptMutability};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::Path;
use std::sync::RwLock;

use crate::domain::model::{
    APIEndpoint, Aggregate, ArchitecturalDecision, ArchitecturalRule, BoundedContext, CallEdge,
    Conventions, DecisionStatus, DomainEvent, DomainModel, Entity, ExternalSystem, Field,
    ImportEdge, Method, Module, Ownership, Policy, PolicyKind, ReadModel, ReferenceEdge,
    Repository, Service, ServiceKind, SourceFile, SymbolDef, TechStack, ValueObject,
};

pub(crate) const ACTUAL_STATE: &str = "actual";

/// Compatibility names accepted at public/API boundaries.
///
/// Internally Axon persists one implemented graph. Older tool names still refer
/// to desired/planned/current state; normalize those names before reaching store
/// query construction so legacy vocabulary is isolated from Cozo scripts.
pub(crate) fn canonical_model_state(state: &str) -> &str {
    match state {
        "" | "actual" | "implemented" | "current" | "planned" | "desired" => ACTUAL_STATE,
        other => other,
    }
}

pub(crate) fn normalize_query_state_aliases(script: &str) -> String {
    let mut normalized = script.to_string();
    for alias in ["implemented", "current", "planned", "desired"] {
        normalized = normalized
            .replace(
                &format!("state: '{alias}'"),
                &format!("state: '{ACTUAL_STATE}'"),
            )
            .replace(
                &format!("state = '{alias}'"),
                &format!("state = '{ACTUAL_STATE}'"),
            );
    }
    normalized
}

/// Metadata about a stored project.
#[derive(Debug, Clone)]
pub struct ProjectInfo {
    pub workspace_path: String,
    pub project_name: String,
    pub updated_at: String,
}

/// Comprehensive model health report computed via Datalog inference.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelHealth {
    pub score: u32,
    pub circular_deps: Vec<[String; 2]>,
    /// Module-level import cycles (each a strongly-connected set of module paths)
    /// derived from syntactic `import_edge` facts. Complements `circular_deps`,
    /// which only covers cycles between bounded contexts.
    pub module_cycles: Vec<Vec<String>>,
    pub layer_violations: Vec<LayerViolation>,
    pub missing_invariants: Vec<[String; 2]>,
    pub orphan_contexts: Vec<String>,
    pub god_contexts: Vec<String>,
    pub unsourced_events: Vec<[String; 2]>,
    pub complexity: Vec<ContextComplexity>,
    pub policy_coverage: PolicyCoverage,
    pub policy_violations: Vec<serde_json::Value>,
    pub bottleneck_contexts: Vec<String>,
    pub communities: Vec<CommunityMembership>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LayerViolation {
    pub context: String,
    pub domain_service: String,
    pub infra_dependency: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ContextComplexity {
    pub context: String,
    pub entity_count: u32,
    pub service_count: u32,
    pub event_count: u32,
    pub dep_count: u32,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PolicyCoverage {
    pub context_count: usize,
    pub layer_assignment_count: usize,
    pub dependency_constraint_count: usize,
    pub missing_layer_assignments: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CommunityMembership {
    pub context: String,
    pub community: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct FactSnapshotSummary {
    pub knowledge_kind: String,
    pub state: String,
    pub available: bool,
    pub snapshot_timestamp_us: Option<i64>,
    pub context_count: usize,
    pub entity_count: usize,
    pub value_object_count: usize,
    pub service_count: usize,
    pub repository_count: usize,
    pub event_count: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DriftFreshness {
    pub available: bool,
    pub status: String,
    pub computed_at_us: Option<i64>,
    pub basis_timestamp_us: Option<i64>,
    pub entry_count: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TruthMaintenanceReport {
    pub asserted: FactSnapshotSummary,
    pub scanned: FactSnapshotSummary,
    pub drift: DriftFreshness,
    pub assumptions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningProvenance {
    pub source: String,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningDerivation {
    pub rule: String,
    #[serde(default)]
    pub derived_from: Vec<String>,
    pub witness_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningAssumption {
    pub assumption_kind: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningSupportEdge {
    pub support_kind: String,
    pub summary: String,
    pub detail: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningDependency {
    pub dependency_kind: String,
    pub dependency_state: String,
    pub basis_timestamp_us: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningJustification {
    pub fact_kind: String,
    pub fact_key: String,
    pub fact_state: String,
    pub basis_timestamp_us: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningFactRef {
    pub fact_kind: String,
    pub fact_key: String,
    pub fact_state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedReasoningClaim {
    pub claim_id: String,
    pub claim_kind: String,
    pub subject: String,
    pub status: String,
    pub summary: String,
    pub payload: Value,
    pub provenance: ReasoningProvenance,
    pub stale: bool,
    pub computed_at_us: i64,
    #[serde(default)]
    pub derivations: Vec<ReasoningDerivation>,
    #[serde(default)]
    pub assumptions: Vec<ReasoningAssumption>,
    #[serde(default)]
    pub supports: Vec<ReasoningSupportEdge>,
    #[serde(default)]
    pub dependencies: Vec<ReasoningDependency>,
    #[serde(default)]
    pub justifications: Vec<ReasoningJustification>,
}

impl PersistedReasoningClaim {
    pub fn proof_json(&self) -> Option<Value> {
        match self.derivations.as_slice() {
            [] => None,
            [single] => Some(json!({
                "rule": single.rule,
                "derived_from": single.derived_from,
                "witness_count": single.witness_count,
            })),
            many => Some(Value::Array(
                many.iter()
                    .map(|derivation| {
                        json!({
                            "rule": derivation.rule,
                            "derived_from": derivation.derived_from,
                            "witness_count": derivation.witness_count,
                        })
                    })
                    .collect(),
            )),
        }
    }

    pub fn evidence_json(&self) -> Option<Value> {
        match self.supports.as_slice() {
            [] => None,
            [single] => Some(single.detail.clone()),
            many => Some(Value::Array(
                many.iter()
                    .map(|support| {
                        json!({
                            "support_kind": support.support_kind,
                            "summary": support.summary,
                            "detail": support.detail,
                        })
                    })
                    .collect(),
            )),
        }
    }

    pub fn limitation_texts(&self) -> Vec<String> {
        self.assumptions
            .iter()
            .filter(|assumption| assumption.assumption_kind == "limitation")
            .map(|assumption| assumption.text.clone())
            .collect()
    }

    pub fn assumption_texts(&self) -> Vec<String> {
        self.assumptions
            .iter()
            .filter(|assumption| assumption.assumption_kind != "limitation")
            .map(|assumption| assumption.text.clone())
            .collect()
    }
}

/// CozoDB-backed cerebral store for domain models.
///
/// Architecture:
/// - Every domain element is stored as a **first-class relational tuple**.
/// - Sub-structures (fields, methods, parameters, invariants, validation rules)
///   are their own relations — not JSON blobs. Datalog can reason about them directly.
/// - Domain/source relations use Cozo `Validity` for point-in-time actual-state history.
/// - Diffs are temporal comparisons over the implemented graph, not desired-vs-actual slices.
/// - `DomainModel` structs are reconstructed on-demand from relations.
pub struct Store {
    db: DbInstance,
    operation_lock: RwLock<()>,
}

thread_local! {
    static STORE_LOCK_DEPTH: Cell<usize> = const { Cell::new(0) };
}

struct StoreLockDepthGuard;

impl StoreLockDepthGuard {
    fn enter() -> Self {
        STORE_LOCK_DEPTH.with(|depth| depth.set(depth.get() + 1));
        Self
    }
}

impl Drop for StoreLockDepthGuard {
    fn drop(&mut self) {
        STORE_LOCK_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

type PolicySnapshot = (
    BTreeSet<(String, String)>,
    BTreeSet<(String, String, String, String)>,
);

type PracticeSymbolAlias = (String, String, String, i64, String);

pub(crate) type SharedField = (String, String, String, String, String);

impl Store {
    /// Open an in-memory store.
    ///
    /// The path parameter is retained for callers that still derive a crate-local
    /// store location, but CozoDB data now lives only for the process lifetime.
    pub fn open(path: &Path) -> Result<Self> {
        let root = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let db = DbInstance::new("mem", "", Default::default())
            .map_err(|e| anyhow::anyhow!("Failed to open in-memory CozoDB: {:?}", e))?;

        Self::init_schema(&db)?;
        let store = Self {
            db,
            operation_lock: RwLock::new(()),
        };
        let ws = canonicalize_path(&root.to_string_lossy());
        // Seed conventional Clean-Architecture rules in memory. Runtime
        // overrides can replace these rows for the active store session.
        store.seed_default_constraints(&ws)?;
        Ok(store)
    }

    // ── Schema ─────────────────────────────────────────────────────────────

    fn init_schema(db: &DbInstance) -> Result<()> {
        // Migration v0: old schema used 'workspace_path' key on project
        let has_v0 = db
            .run_script(
                "?[x] := *project{workspace_path: x}",
                Default::default(),
                ScriptMutability::Immutable,
            )
            .is_ok();

        if has_v0 {
            let old_tables = [
                "project",
                "context",
                "context_dep",
                "entity",
                "entity_field",
                "entity_method",
                "method_param",
                "invariant",
                "service",
                "service_dep",
                "service_method",
                "event",
                "event_field",
                "value_object",
                "repository",
                "arch_rule",
                "live_import",
            ];
            for t in old_tables {
                let _ = db.run_script(
                    &format!("::remove {t}"),
                    Default::default(),
                    ScriptMutability::Mutable,
                );
            }
        }

        // Migration v1: schema had *_json blob columns on entity/service/event/etc.
        let has_v1 = db
            .run_script(
                "?[x] := *entity{fields_json: x}",
                Default::default(),
                ScriptMutability::Immutable,
            )
            .is_ok();

        if has_v1 {
            for t in ["entity", "service", "event", "value_object", "repository"] {
                let _ = db.run_script(
                    &format!("::remove {t}"),
                    Default::default(),
                    ScriptMutability::Mutable,
                );
            }
        }

        // Migration v2: tables lacked file_path/start_line/end_line columns
        let needs_v2 = db
            .run_script(
                "?[x] := *service{file_path: x}",
                Default::default(),
                ScriptMutability::Immutable,
            )
            .is_err()
            && db
                .run_script(
                    "?[x] := *service{name: x}",
                    Default::default(),
                    ScriptMutability::Immutable,
                )
                .is_ok();

        if needs_v2 {
            for t in [
                "entity",
                "service",
                "event",
                "value_object",
                "repository",
                "module",
            ] {
                let _ = db.run_script(
                    &format!("::remove {t}"),
                    Default::default(),
                    ScriptMutability::Mutable,
                );
            }
        }

        // Migration v3: schema lacked Validity columns for time-travel
        let needs_v3 = db
            .run_script(
                "?[x] := *context{workspace: x}",
                Default::default(),
                ScriptMutability::Immutable,
            )
            .is_ok()
            && db
                .run_script(
                    "?[x] := *context{workspace: x @ 'NOW'}",
                    Default::default(),
                    ScriptMutability::Immutable,
                )
                .is_err();

        if needs_v3 {
            let temporal_tables = [
                "context",
                "context_dep",
                "owner_meta",
                "aggregate",
                "aggregate_member",
                "entity",
                "policy",
                "policy_link",
                "read_model",
                "service",
                "service_dep",
                "event",
                "value_object",
                "repository",
                "module",
                "external_system",
                "external_system_context",
                "api_endpoint",
                "invokes_endpoint",
                "calls_external_system",
                "architectural_decision",
                "decision_context",
                "decision_consequence",
                "invariant",
                "field",
                "method",
                "method_param",
                "vo_rule",
                "ast_edge",
                "source_file",
                "symbol",
                "import_edge",
                "reference_edge",
            ];
            for t in temporal_tables {
                let _ = db.run_script(
                    &format!("::remove {t}"),
                    Default::default(),
                    ScriptMutability::Mutable,
                );
            }
        }

        let schemas = vec![
            // Project metadata (rules/tech/conventions as JSON — config, not domain topology)
            ":create project { workspace: String => name: String, description: String default '', updated_at: String, rules_json: String default '[]', tech_stack_json: String default '{}', conventions_json: String default '{}' }",
            // ── Domain element headers (all with Validity for actual-state time-travel) ──
            ":create context { workspace: String, name: String, state: String, vld: Validity default 'ASSERT' => description: String default '', module_path: String default '' }",
            ":create context_dep { workspace: String, from_ctx: String, to_ctx: String, state: String, vld: Validity default 'ASSERT' }",
            ":create owner_meta { workspace: String, context: String, owner_kind: String, owner: String, state: String, vld: Validity default 'ASSERT' => team: String default '', owners_json: String default '[]', rationale: String default '' }",
            ":create aggregate { workspace: String, context: String, name: String, state: String, vld: Validity default 'ASSERT' => description: String default '', root_entity: String default '' }",
            ":create aggregate_member { workspace: String, context: String, aggregate: String, member_kind: String, member: String, state: String, vld: Validity default 'ASSERT' }",
            ":create entity { workspace: String, context: String, name: String, state: String, vld: Validity default 'ASSERT' => description: String default '', aggregate_root: Bool default false, file_path: String default '', start_line: Int default 0, end_line: Int default 0 }",
            ":create policy { workspace: String, context: String, name: String, state: String, vld: Validity default 'ASSERT' => description: String default '', kind: String default 'domain' }",
            ":create policy_link { workspace: String, context: String, policy: String, link_kind: String, link: String, idx: Int, state: String, vld: Validity default 'ASSERT' }",
            ":create read_model { workspace: String, context: String, name: String, state: String, vld: Validity default 'ASSERT' => description: String default '', source: String default '' }",
            ":create service { workspace: String, context: String, name: String, state: String, vld: Validity default 'ASSERT' => description: String default '', kind: String default 'domain', file_path: String default '', start_line: Int default 0, end_line: Int default 0 }",
            ":create service_dep { workspace: String, context: String, service: String, dep: String, state: String, vld: Validity default 'ASSERT' }",
            ":create event { workspace: String, context: String, name: String, state: String, vld: Validity default 'ASSERT' => description: String default '', source: String default '', file_path: String default '', start_line: Int default 0, end_line: Int default 0 }",
            ":create value_object { workspace: String, context: String, name: String, state: String, vld: Validity default 'ASSERT' => description: String default '', file_path: String default '', start_line: Int default 0, end_line: Int default 0 }",
            ":create repository { workspace: String, context: String, name: String, state: String, vld: Validity default 'ASSERT' => aggregate: String default '', file_path: String default '', start_line: Int default 0, end_line: Int default 0 }",
            ":create module { workspace: String, context: String, name: String, state: String, vld: Validity default 'ASSERT' => path: String default '', public: Bool default false, file_path: String default '', description: String default '' }",
            ":create external_system { workspace: String, name: String, state: String, vld: Validity default 'ASSERT' => description: String default '', kind: String default '', rationale: String default '' }",
            ":create external_system_context { workspace: String, system: String, context: String, idx: Int, state: String, vld: Validity default 'ASSERT' }",
            ":create api_endpoint { workspace: String, context: String, id: String, state: String, vld: Validity default 'ASSERT' => service_id: String default '', method: String default '', route_pattern: String default '', description: String default '' }",
            ":create invokes_endpoint { workspace: String, caller_context: String, caller_method: String, endpoint_id: String, state: String, vld: Validity default 'ASSERT' }",
            ":create calls_external_system { workspace: String, caller_context: String, caller_method: String, ext_id: String, state: String, vld: Validity default 'ASSERT' }",
            ":create architectural_decision { workspace: String, id: String, state: String, vld: Validity default 'ASSERT' => title: String default '', status: String default 'proposed', scope: String default '', date: String default '', rationale: String default '' }",
            ":create decision_context { workspace: String, decision_id: String, context: String, idx: Int, state: String, vld: Validity default 'ASSERT' }",
            ":create decision_consequence { workspace: String, decision_id: String, idx: Int, state: String, vld: Validity default 'ASSERT' => text: String default '' }",
            // ── First-class sub-structures ──
            ":create invariant { workspace: String, context: String, entity: String, idx: Int, state: String, vld: Validity default 'ASSERT' => text: String }",
            ":create field { workspace: String, context: String, owner_kind: String, owner: String, name: String, state: String, vld: Validity default 'ASSERT' => field_type: String default '', required: Bool default false, description: String default '', idx: Int default 0 }",
            ":create method { workspace: String, context: String, owner_kind: String, owner: String, name: String, state: String, vld: Validity default 'ASSERT' => description: String default '', return_type: String default '', idx: Int default 0 }",
            ":create method_param { workspace: String, context: String, owner_kind: String, owner: String, method: String, name: String, state: String, vld: Validity default 'ASSERT' => param_type: String default '', required: Bool default false, description: String default '', idx: Int default 0 }",
            ":create vo_rule { workspace: String, context: String, value_object: String, idx: Int, state: String, vld: Validity default 'ASSERT' => text: String }",
            // ── Architecture policy relations (no state, no Validity) ──
            ":create layer_assignment { workspace: String, context: String => layer: String }",
            ":create dependency_constraint { workspace: String, constraint_kind: String, source: String, target: String => rule: String default 'forbidden' }",
            // Ephemeral — no state column
            ":create live_import { workspace: String, from_file: String, to_module: String }",
            // AST structural edges (extends, implements, decorators)
            ":create ast_edge { workspace: String, from_node: String, to_node: String, edge_type: String, state: String, vld: Validity default 'ASSERT' => file_path: String default '', line: Int default 0 }",
            // Resolved call edges from rust-analyzer (name resolution + type
            // inference): which concrete definition a call site actually targets —
            // something the syn scanner cannot determine. Populated by the rust_scan semantic phase.
            ":create resolved_call { workspace: String, caller: String, callee: String, callee_file: String, state: String, vld: Validity default 'ASSERT' => callee_line: Int default 0, caller_file: String default '', caller_line: Int default 0, call_site_line: Int default 0, call_expr: String default '', dispatch_kind: String default 'unknown' }",
            // ── Source-level relations ──
            ":create source_file { workspace: String, path: String, state: String, vld: Validity default 'ASSERT' => context: String default '', language: String default '' }",
            ":create symbol { workspace: String, name: String, state: String, vld: Validity default 'ASSERT' => kind: String default '', context: String default '', file_path: String default '', start_line: Int default 0, end_line: Int default 0, visibility: String default 'public' }",
            ":create import_edge { workspace: String, from_file: String, to_module: String, state: String, vld: Validity default 'ASSERT' => context: String default '' }",
            ":create reference_edge { workspace: String, from_file: String, to_path: String, reference_kind: String, line: Int, state: String, vld: Validity default 'ASSERT' => context: String default '' }",
            // ── Symbol-level call graph ──
            ":create calls_symbol { workspace: String, caller: String, callee: String, state: String, vld: Validity default 'ASSERT' => file_path: String default '', line: Int default 0, context: String default '' }",
            // ── Drift model ──
            ":create drift { workspace: String, category: String, context: String, name: String, change_type: String, vld: Validity default 'ASSERT' => detail: String default '' }",
            ":create drift_meta { workspace: String => computed_at_us: Int default 0 }",
            // ── Reasoning kernel relations (non-temporal, current cache only) ──
            ":create reasoning_claim { workspace: String, claim_id: String => claim_kind: String default '', subject: String default '', status: String default '', summary: String default '', payload_json: String default '{}', provenance_source: String default '', provenance_state: String default '', stale: Bool default true, computed_at_us: Int default 0 }",
            ":create reasoning_derivation { workspace: String, claim_id: String, idx: Int => rule: String default '', derived_from_json: String default '[]', witness_count: Int default 0 }",
            ":create reasoning_assumption { workspace: String, claim_id: String, idx: Int => assumption_kind: String default 'assumption', text: String default '' }",
            ":create reasoning_support { workspace: String, claim_id: String, idx: Int => support_kind: String default '', summary: String default '', detail_json: String default '{}' }",
            ":create reasoning_dependency { workspace: String, claim_id: String, idx: Int => dependency_kind: String default '', dependency_state: String default '', basis_timestamp_us: Int default 0 }",
            ":create reasoning_justification { workspace: String, claim_id: String, idx: Int => fact_kind: String default '', fact_key: String default '', fact_state: String default '', basis_timestamp_us: Int default 0 }",
            // ── Snapshot log (explicit timestamp tracking for list_snapshots) ──
            ":create snapshot_log { workspace: String, state: String, timestamp_us: Int => label: String default '' }",
        ];

        for schema in schemas {
            db.run_script(schema, Default::default(), ScriptMutability::Mutable)
                .map_err(|e| {
                    anyhow::anyhow!("Failed to create Cozo schema relation from `{schema}`: {e:?}")
                })?;
        }

        // ── Secondary indices ──
        // CozoDB indices are reordered stored relations, queryable directly.
        // They avoid full scans for reverse lookups and non-primary-key filters.
        let indices = [
            // Reverse context dependency: "who depends on me?"
            "::index create context_dep:reverse {to_ctx}",
            // Reverse service dependency: "who uses this service?"
            "::index create service_dep:reverse {dep}",
            // Find events by their source entity
            "::index create event:by_source {source}",
            // Find aggregate members by member name
            "::index create aggregate_member:by_member {member_kind, member}",
            // Find fields/methods by owner kind + owner
            "::index create field:by_owner {owner_kind, owner}",
            "::index create method:by_owner {owner_kind, owner}",
            // Reverse AST edges: "what points to this node?"
            "::index create ast_edge:reverse {to_node, edge_type}",
            // Context by module_path for live dependency matching
            "::index create context:by_module_path {module_path}",
            // Owners by owner_kind + owner
            "::index create owner_meta:by_owner {owner_kind, owner}",
            // External system contexts by context
            "::index create external_system_context:by_context {context}",
            // Calls/invocations by target
            "::index create invokes_endpoint:by_endpoint {endpoint_id}",
            "::index create calls_external_system:by_ext {ext_id}",
            // Source file by context
            "::index create source_file:by_context {context}",
            // Symbol by context + kind
            "::index create symbol:by_context {context, kind}",
            // Symbol by file_path (find all symbols in a file)
            "::index create symbol:by_file {file_path}",
            // Import edge by target module (reverse lookup)
            "::index create import_edge:by_target {to_module}",
            // Import edge by context
            "::index create import_edge:by_context {context}",
            // Reference edge by target path and context
            "::index create reference_edge:by_target {to_path}",
            "::index create reference_edge:by_context {context}",
            // Call graph: reverse lookup (who calls this symbol?)
            "::index create calls_symbol:by_callee {callee}",
            // Call graph: by context
            "::index create calls_symbol:by_context {context}",
        ];
        for idx in indices {
            db.run_script(idx, Default::default(), ScriptMutability::Mutable)
                .map_err(|e| {
                    anyhow::anyhow!("Failed to create Cozo secondary index from `{idx}`: {e:?}")
                })?;
        }

        // ── Full-text search indices ──
        // CozoDB FTS enables keyword search across description and text fields.
        let fts_indices = [
            "::fts create context:fts {
                extractor: description,
                extract_filter: description != '',
                tokenizer: Simple,
                filters: [Lowercase]
            }",
            "::fts create entity:fts {
                extractor: description,
                extract_filter: description != '',
                tokenizer: Simple,
                filters: [Lowercase]
            }",
            "::fts create service:fts {
                extractor: description,
                extract_filter: description != '',
                tokenizer: Simple,
                filters: [Lowercase]
            }",
            "::fts create event:fts {
                extractor: description,
                extract_filter: description != '',
                tokenizer: Simple,
                filters: [Lowercase]
            }",
            "::fts create architectural_decision:title_fts {
                extractor: title,
                extract_filter: title != '',
                tokenizer: Simple,
                filters: [Lowercase]
            }",
            "::fts create architectural_decision:rationale_fts {
                extractor: rationale,
                extract_filter: rationale != '',
                tokenizer: Simple,
                filters: [Lowercase]
            }",
            "::fts create invariant:text_fts {
                extractor: text,
                tokenizer: Simple,
                filters: [Lowercase]
            }",
        ];
        for idx in fts_indices {
            db.run_script(idx, Default::default(), ScriptMutability::Mutable)
                .map_err(|e| {
                    anyhow::anyhow!("Failed to create Cozo full-text index from `{idx}`: {e:?}")
                })?;
        }

        Ok(())
    }

    // ── Core State Operations ──────────────────────────────────────────────

    /// Compatibility alias for saving the current implemented model.
    pub fn save_desired(&self, workspace_path: &str, model: &DomainModel) -> Result<()> {
        let ws = canonicalize_path(workspace_path);
        self.save_state(&ws, model, ACTUAL_STATE)
    }

    /// Compatibility alias for loading the current implemented model.
    pub fn load_desired(&self, workspace_path: &str) -> Result<Option<DomainModel>> {
        self.reconstruct_model(workspace_path, ACTUAL_STATE)
    }

    /// Load the actual domain model (reconstructed from relations).
    pub fn load_actual(&self, workspace_path: &str) -> Result<Option<DomainModel>> {
        self.reconstruct_model(workspace_path, ACTUAL_STATE)
    }

    /// Save a scanned model as the actual state (from AST extraction).
    pub fn save_actual(&self, workspace_path: &str, model: &DomainModel) -> Result<()> {
        let ws = canonicalize_path(workspace_path);
        self.with_write_lock(|| self.save_state(&ws, model, ACTUAL_STATE))
    }

    /// Save a scanned actual model and refresh its temporal drift in one store operation.
    pub fn save_actual_and_compute_drift(
        &self,
        workspace_path: &str,
        model: &DomainModel,
    ) -> Result<usize> {
        self.with_write_lock(|| {
            self.save_actual(workspace_path, model)?;
            self.compute_drift(workspace_path)
        })
    }

    /// Save one unified scan result: the source-extracted actual model plus any
    /// compiler-resolved semantic call edges from the same scan generation.
    pub fn save_actual_scan_and_compute_drift(
        &self,
        workspace_path: &str,
        scan: &crate::domain::analyze::ActualScan,
    ) -> Result<usize> {
        self.with_write_lock(|| {
            self.save_actual(workspace_path, &scan.model)?;
            // Even on semantic-resolution failure, `resolved_calls` is empty;
            // persisting it retracts stale resolved_call rows from earlier scans.
            self.save_resolved_calls(workspace_path, &scan.resolved_calls)?;
            self.compute_drift(workspace_path)
        })
    }

    /// Record a temporal checkpoint for the current implemented graph.
    pub fn record_actual_snapshot(&self, workspace_path: &str) -> Result<i64> {
        let ws = canonicalize_path(workspace_path);
        self.with_write_lock(|| self.record_snapshot(&ws, ACTUAL_STATE))
    }

    /// Persist resolved call edges from rust-analyzer, replacing any previous
    /// generation (retract-then-assert) so stale edges don't linger.
    pub fn save_resolved_calls(
        &self,
        workspace_path: &str,
        calls: &[crate::domain::rust_analyzer::ResolvedCall],
    ) -> Result<usize> {
        self.with_write_lock(|| {
            let ws = canonicalize_path(workspace_path);
            let sv = |x: &str| cozo::DataValue::Str(x.into());

            // Retract the prior set. An empty relation derives no rows → harmless no-op.
            self.run_script(
                                "?[workspace, caller, callee, callee_file, state, vld] := \
                                     *resolved_call{workspace, caller, callee, callee_file, state: 'actual' @ 'NOW'}, \
                                     state = 'actual', \
                   workspace = $ws, vld = 'RETRACT' \
                                 :put resolved_call { workspace, caller, callee, callee_file, state, vld }",
                params_map(&[("ws", &ws)]),
                ScriptMutability::Mutable,
            )
            .map_err(|e| anyhow::anyhow!("retract resolved_call: {e:?}"))?;

            let rows: Vec<cozo::DataValue> = calls
                .iter()
                .map(|c| {
                    cozo::DataValue::List(vec![
                        sv(&ws),
                        sv(&c.caller),
                        sv(&c.callee),
                        sv(&c.callee_file),
                        sv(ACTUAL_STATE),
                        cozo::DataValue::from(usize_to_i64(c.callee_line)),
                        sv(&c.caller_file),
                        cozo::DataValue::from(usize_to_i64(c.caller_line)),
                        cozo::DataValue::from(usize_to_i64(c.call_site_line)),
                        sv(&c.call_expr),
                        sv(&c.dispatch_kind),
                    ])
                })
                .collect();
            self.batch_put(
                rows,
                "?[workspace, caller, callee, callee_file, state, callee_line, caller_file, caller_line, call_site_line, call_expr, dispatch_kind] <- $rows \
                 :put resolved_call { workspace, caller, callee, callee_file, state, callee_line, caller_file, caller_line, call_site_line, call_expr, dispatch_kind }",
                "save resolved_calls",
            )?;
            Ok(calls.len())
        })
    }

    fn run_mutation_script(
        &self,
        script: &str,
        params: BTreeMap<String, cozo::DataValue>,
        context: impl Into<String>,
    ) -> Result<()> {
        let context = context.into();
        self.run_script(script, params, ScriptMutability::Mutable)
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!("{context}: {:?}", e))
    }

    /// Insert many rows with a single `:put` instead of one script per row.
    ///
    /// `rows` is one `DataValue::List` per row; `script` must bind them via
    /// `<- $rows`. An empty input is a no-op (an empty `<-` is rejected by
    /// CozoDB). This collapses thousands of per-row script executions into one,
    /// which dominates `save_actual` cost on call-heavy graphs.
    fn batch_put(
        &self,
        rows: Vec<cozo::DataValue>,
        script: &str,
        context: impl Into<String>,
    ) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut params = BTreeMap::new();
        params.insert("rows".to_string(), cozo::DataValue::List(rows));
        self.run_script(script, params, ScriptMutability::Mutable)
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!("{}: {:?}", context.into(), e))
    }

    fn run_script(
        &self,
        script: &str,
        params: BTreeMap<String, cozo::DataValue>,
        mutability: ScriptMutability,
    ) -> std::result::Result<cozo::NamedRows, cozo::Error> {
        match mutability {
            ScriptMutability::Mutable => {
                self.with_write_lock(|| self.db.run_script(script, params, mutability))
            }
            ScriptMutability::Immutable => {
                self.with_read_lock(|| self.db.run_script(script, params, mutability))
            }
        }
    }

    fn with_read_lock<T>(&self, f: impl FnOnce() -> T) -> T {
        if STORE_LOCK_DEPTH.with(|depth| depth.get() > 0) {
            return f();
        }
        let _operation_guard = self
            .operation_lock
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _depth_guard = StoreLockDepthGuard::enter();
        f()
    }

    fn with_write_lock<T>(&self, f: impl FnOnce() -> T) -> T {
        if STORE_LOCK_DEPTH.with(|depth| depth.get() > 0) {
            return f();
        }
        let _operation_guard = self
            .operation_lock
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _depth_guard = StoreLockDepthGuard::enter();
        f()
    }

    fn save_project_metadata(&self, workspace: &str, model: &DomainModel) -> Result<()> {
        let now = chrono_now();
        let rules_json = serde_json::to_string(&model.rules).unwrap_or_else(|_| "[]".into());
        let tech_json = serde_json::to_string(&model.tech_stack).unwrap_or_else(|_| "{}".into());
        let conv_json = serde_json::to_string(&model.conventions).unwrap_or_else(|_| "{}".into());
        let params = params_map(&[
            ("ws", workspace),
            ("name", &model.name),
            ("desc", &model.description),
            ("now", &now),
            ("rules", &rules_json),
            ("tech", &tech_json),
            ("conv", &conv_json),
        ]);
        self.run_mutation_script(
            "?[workspace, name, description, updated_at, rules_json, tech_stack_json, conventions_json] <- \
                [[$ws, $name, $desc, $now, $rules, $tech, $conv]] \
             :put project { workspace => name, description, updated_at, rules_json, tech_stack_json, conventions_json }",
            params,
            format!("save project metadata '{}'", model.name),
        )
    }

    /// Compatibility no-op: actual-first storage has no desired graph to promote.
    pub fn accept(&self, workspace_path: &str) -> Result<()> {
        self.invalidate_reasoning_claims_for_dependency(workspace_path, ACTUAL_STATE)?;
        Ok(())
    }

    /// Compatibility no-op: actual-first storage returns the current implemented model.
    pub fn reset(&self, workspace_path: &str) -> Result<Option<DomainModel>> {
        self.invalidate_reasoning_claims_for_dependency(workspace_path, ACTUAL_STATE)?;
        self.load_actual(workspace_path)
    }

    // ── Private: Sub-structure Helpers ──────────────────────────────────────

    /// Save a slice of fields into the `field` relation.
    fn save_fields(
        &self,
        ws: &str,
        ctx: &str,
        owner_kind: &str,
        owner: &str,
        fields: &[Field],
        state: &str,
    ) -> Result<()> {
        for (i, f) in fields.iter().enumerate() {
            let mut params = params_map(&[
                ("ws", ws),
                ("ctx", ctx),
                ("ok", owner_kind),
                ("ow", owner),
                ("name", &f.name),
                ("st", state),
                ("ft", &f.field_type),
                ("desc", &f.description),
            ]);
            params.insert("req".into(), cozo::DataValue::Bool(f.required));
            params.insert("idx".into(), int_dv(usize_to_i64(i)));
            self
                .run_script(
                    "?[workspace, context, owner_kind, owner, name, state, field_type, required, description, idx] <- \
                        [[$ws, $ctx, $ok, $ow, $name, $st, $ft, $req, $desc, $idx]] \
                     :put field { workspace, context, owner_kind, owner, name, state => field_type, required, description, idx }",
                    params,
                    ScriptMutability::Mutable,
                )
                .map_err(|e| anyhow::anyhow!("save field '{}'.{}: {:?}", owner, f.name, e))?;
        }
        Ok(())
    }

    /// Save a slice of methods (+ their params) into the `method` and `method_param` relations.
    fn save_methods(
        &self,
        ws: &str,
        ctx: &str,
        owner_kind: &str,
        owner: &str,
        methods: &[Method],
        state: &str,
    ) -> Result<()> {
        for (i, m) in methods.iter().enumerate() {
            let mut params = params_map(&[
                ("ws", ws),
                ("ctx", ctx),
                ("ok", owner_kind),
                ("ow", owner),
                ("name", &m.name),
                ("st", state),
                ("desc", &m.description),
                ("rt", &m.return_type),
            ]);
            params.insert("idx".into(), int_dv(usize_to_i64(i)));
            self
                .run_script(
                    "?[workspace, context, owner_kind, owner, name, state, description, return_type, idx] <- \
                        [[$ws, $ctx, $ok, $ow, $name, $st, $desc, $rt, $idx]] \
                     :put method { workspace, context, owner_kind, owner, name, state => description, return_type, idx }",
                    params,
                    ScriptMutability::Mutable,
                )
                .map_err(|e| anyhow::anyhow!("save method '{}'.{}: {:?}", owner, m.name, e))?;

            for (j, p) in m.parameters.iter().enumerate() {
                let mut pp = params_map(&[
                    ("ws", ws),
                    ("ctx", ctx),
                    ("ok", owner_kind),
                    ("ow", owner),
                    ("method", &m.name),
                    ("name", &p.name),
                    ("st", state),
                    ("pt", &p.field_type),
                    ("desc", &p.description),
                ]);
                pp.insert("req".into(), cozo::DataValue::Bool(p.required));
                pp.insert("idx".into(), int_dv(usize_to_i64(j)));
                self
                    .run_script(
                        "?[workspace, context, owner_kind, owner, method, name, state, param_type, required, description, idx] <- \
                            [[$ws, $ctx, $ok, $ow, $method, $name, $st, $pt, $req, $desc, $idx]] \
                         :put method_param { workspace, context, owner_kind, owner, method, name, state => param_type, required, description, idx }",
                        pp,
                        ScriptMutability::Mutable,
                    )
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "save method_param '{}'.{}.{}: {:?}",
                            owner,
                            m.name,
                            p.name,
                            e
                        )
                    })?;
            }
        }
        Ok(())
    }

    fn save_owner_meta(
        &self,
        ws: &str,
        ctx: &str,
        owner_kind: &str,
        owner: &str,
        ownership: &Ownership,
        state: &str,
    ) -> Result<()> {
        let owners_json = serde_json::to_string(&ownership.owners).unwrap_or_else(|_| "[]".into());
        self
            .run_script(
                "?[workspace, context, owner_kind, owner, state, team, owners_json, rationale] <- [[$ws, $ctx, $ok, $owner, $st, $team, $owners, $rationale]] :put owner_meta { workspace, context, owner_kind, owner, state => team, owners_json, rationale }",
                params_map(&[
                    ("ws", ws),
                    ("ctx", ctx),
                    ("ok", owner_kind),
                    ("owner", owner),
                    ("st", state),
                    ("team", &ownership.team),
                    ("owners", &owners_json),
                    ("rationale", &ownership.rationale),
                ]),
                ScriptMutability::Mutable,
            )
            .map_err(|e| anyhow::anyhow!("save owner_meta '{}':'{}': {:?}", owner_kind, owner, e))?;
        Ok(())
    }

    fn remove_owner_meta(&self, ws: &str, ctx: &str, owner_kind: &str, owner: &str) -> Result<()> {
        self.run_mutation_script(
            "?[workspace, context, owner_kind, owner, state, vld] := *owner_meta{workspace, context, owner_kind, owner, state @ 'NOW'}, workspace = $ws, context = $ctx, owner_kind = $ok, owner = $owner, vld = 'RETRACT' :put owner_meta { workspace, context, owner_kind, owner, state, vld }",
            params_map(&[("ws", ws), ("ctx", ctx), ("ok", owner_kind), ("owner", owner)]),
            format!("remove owner_meta {owner_kind}:{owner}"),
        )
    }

    fn replace_owner_fields(
        &self,
        ws: &str,
        ctx: &str,
        owner_kind: &str,
        owner: &str,
        fields: &[Field],
    ) -> Result<()> {
        self.run_mutation_script(
            "?[workspace, context, owner_kind, owner, name, state, vld] := *field{workspace, context, owner_kind, owner, name, state @ 'NOW'}, workspace = $ws, context = $ctx, owner_kind = $ok, owner = $owner, state = 'actual', vld = 'RETRACT' :put field { workspace, context, owner_kind, owner, name, state, vld }",
            params_map(&[("ws", ws), ("ctx", ctx), ("ok", owner_kind), ("owner", owner)]),
            format!("replace fields for {owner_kind}:{owner}"),
        )?;
        self.save_fields(ws, ctx, owner_kind, owner, fields, "actual")
    }

    fn replace_owner_methods(
        &self,
        ws: &str,
        ctx: &str,
        owner_kind: &str,
        owner: &str,
        methods: &[Method],
    ) -> Result<()> {
        self.run_mutation_script(
            "?[workspace, context, owner_kind, owner, name, state, vld] := *method{workspace, context, owner_kind, owner, name, state @ 'NOW'}, workspace = $ws, context = $ctx, owner_kind = $ok, owner = $owner, state = 'actual', vld = 'RETRACT' :put method { workspace, context, owner_kind, owner, name, state, vld }",
            params_map(&[("ws", ws), ("ctx", ctx), ("ok", owner_kind), ("owner", owner)]),
            format!("replace methods for {owner_kind}:{owner}"),
        )?;
        self.run_mutation_script(
            "?[workspace, context, owner_kind, owner, method, name, state, vld] := *method_param{workspace, context, owner_kind, owner, method, name, state @ 'NOW'}, workspace = $ws, context = $ctx, owner_kind = $ok, owner = $owner, state = 'actual', vld = 'RETRACT' :put method_param { workspace, context, owner_kind, owner, method, name, state, vld }",
            params_map(&[("ws", ws), ("ctx", ctx), ("ok", owner_kind), ("owner", owner)]),
            format!("replace method params for {owner_kind}:{owner}"),
        )?;
        self.save_methods(ws, ctx, owner_kind, owner, methods, "actual")
    }

    fn replace_invariants(
        &self,
        ws: &str,
        ctx: &str,
        entity: &str,
        invariants: &[String],
    ) -> Result<()> {
        self.run_mutation_script(
                "?[workspace, context, entity, idx, state, text, vld] := *invariant{workspace, context, entity, idx, state, text @ 'NOW'}, workspace = $ws, context = $ctx, entity = $entity, state = 'actual', vld = 'RETRACT' :put invariant { workspace, context, entity, idx, state, vld => text }",
            params_map(&[("ws", ws), ("ctx", ctx), ("entity", entity)]),
            format!("replace invariants for entity:{entity}"),
        )?;
        for (idx, invariant) in invariants.iter().enumerate() {
            let mut params = params_map(&[
                ("ws", ws),
                ("ctx", ctx),
                ("entity", entity),
                ("text", invariant),
            ]);
            params.insert("idx".into(), int_dv(usize_to_i64(idx)));
            self.run_script(
                "?[workspace, context, entity, idx, state, text] <- [[$ws, $ctx, $entity, $idx, 'actual', $text]] :put invariant { workspace, context, entity, idx, state => text }",
                params,
                ScriptMutability::Mutable,
            ).map_err(|e| anyhow::anyhow!("replace_invariants '{}': {:?}", entity, e))?;
        }
        Ok(())
    }

    fn replace_vo_rules(
        &self,
        ws: &str,
        ctx: &str,
        value_object: &str,
        rules: &[String],
    ) -> Result<()> {
        self.run_mutation_script(
                "?[workspace, context, value_object, idx, state, text, vld] := *vo_rule{workspace, context, value_object, idx, state, text @ 'NOW'}, workspace = $ws, context = $ctx, value_object = $vo, state = 'actual', vld = 'RETRACT' :put vo_rule { workspace, context, value_object, idx, state, vld => text }",
            params_map(&[("ws", ws), ("ctx", ctx), ("vo", value_object)]),
            format!("replace value object rules for {value_object}"),
        )?;
        for (idx, rule) in rules.iter().enumerate() {
            let mut params = params_map(&[
                ("ws", ws),
                ("ctx", ctx),
                ("vo", value_object),
                ("text", rule),
            ]);
            params.insert("idx".into(), int_dv(usize_to_i64(idx)));
            self.run_script(
                "?[workspace, context, value_object, idx, state, text] <- [[$ws, $ctx, $vo, $idx, 'actual', $text]] :put vo_rule { workspace, context, value_object, idx, state => text }",
                params,
                ScriptMutability::Mutable,
            ).map_err(|e| anyhow::anyhow!("replace_vo_rules '{}': {:?}", value_object, e))?;
        }
        Ok(())
    }

    fn replace_service_deps(
        &self,
        ws: &str,
        ctx: &str,
        service: &str,
        dependencies: &[String],
    ) -> Result<()> {
        self.run_mutation_script(
            "?[workspace, context, service, dep, state, vld] := *service_dep{workspace, context, service, dep, state @ 'NOW'}, workspace = $ws, context = $ctx, service = $service, state = 'actual', vld = 'RETRACT' :put service_dep { workspace, context, service, dep, state, vld }",
            params_map(&[("ws", ws), ("ctx", ctx), ("service", service)]),
            format!("replace service dependencies for {service}"),
        )?;
        for dep in dependencies {
            self.run_script(
                "?[workspace, context, service, dep, state] <- [[$ws, $ctx, $service, $dep, 'actual']] :put service_dep { workspace, context, service, dep, state }",
                params_map(&[("ws", ws), ("ctx", ctx), ("service", service), ("dep", dep)]),
                ScriptMutability::Mutable,
            ).map_err(|e| anyhow::anyhow!("replace_service_deps '{}': {:?}", service, e))?;
        }
        Ok(())
    }

    fn ensure_project(&self, workspace_path: &str) -> Result<()> {
        let ws = canonicalize_path(workspace_path);
        let has_project = self
            .run_script(
                "?[name] := *project{workspace: $ws, name}",
                params_map(&[("ws", &ws)]),
                ScriptMutability::Immutable,
            )
            .map(|r| !r.rows.is_empty())
            .unwrap_or(false);
        if has_project {
            return Ok(());
        }

        let empty = DomainModel::empty(workspace_path);
        self.save_project_metadata(&ws, &empty)
            .map_err(|e| anyhow::anyhow!("ensure_project: {e}"))?;
        self.save_owner_meta(&ws, "", "project", &empty.name, &empty.ownership, "actual")?;
        Ok(())
    }

    /// Query fields for a specific owner from the `field` relation, ordered by idx.
    fn query_fields(
        &self,
        ws: &str,
        ctx: &str,
        owner_kind: &str,
        owner: &str,
        state: &str,
    ) -> Vec<Field> {
        let params = params_map(&[
            ("ws", ws),
            ("ctx", ctx),
            ("ok", owner_kind),
            ("ow", owner),
            ("st", state),
        ]);
        let rows = self
            .run_script(
                "?[name, field_type, required, description, idx] := \
                    *field{workspace: $ws, context: $ctx, owner_kind: $ok, owner: $ow, \
                           name, state: $st, field_type, required, description, idx @ 'NOW'}",
                params,
                ScriptMutability::Immutable,
            )
            .map(|r| r.rows)
            .unwrap_or_default();

        let mut indexed: Vec<(i64, Field)> = rows
            .iter()
            .map(|r| {
                (
                    dv_i64(&r[4]),
                    Field {
                        name: dv_str(&r[0]),
                        field_type: dv_str(&r[1]),
                        required: matches!(&r[2], cozo::DataValue::Bool(true)),
                        description: dv_str(&r[3]),
                    },
                )
            })
            .collect();
        indexed.sort_by_key(|(i, _)| *i);
        indexed.into_iter().map(|(_, f)| f).collect()
    }

    /// Query methods (+ their params) for a specific owner, ordered by idx.
    fn query_methods(
        &self,
        ws: &str,
        ctx: &str,
        owner_kind: &str,
        owner: &str,
        state: &str,
    ) -> Vec<Method> {
        let params = params_map(&[
            ("ws", ws),
            ("ctx", ctx),
            ("ok", owner_kind),
            ("ow", owner),
            ("st", state),
        ]);
        let rows = self
            .run_script(
                "?[name, description, return_type, idx] := \
                    *method{workspace: $ws, context: $ctx, owner_kind: $ok, owner: $ow, \
                            name, state: $st, description, return_type, idx @ 'NOW'}",
                params,
                ScriptMutability::Immutable,
            )
            .map(|r| r.rows)
            .unwrap_or_default();

        let mut indexed: Vec<(i64, Method)> = rows
            .iter()
            .map(|r| {
                let mname = dv_str(&r[0]);
                let mp = params_map(&[
                    ("ws", ws),
                    ("ctx", ctx),
                    ("ok", owner_kind),
                    ("ow", owner),
                    ("method", &mname),
                    ("st", state),
                ]);
                let param_rows = self
                    .run_script(
                        "?[name, param_type, required, description, idx] := \
                            *method_param{workspace: $ws, context: $ctx, owner_kind: $ok, \
                                          owner: $ow, method: $method, name, state: $st, \
                                          param_type, required, description, idx @ 'NOW'}",
                        mp,
                        ScriptMutability::Immutable,
                    )
                    .map(|r| r.rows)
                    .unwrap_or_default();

                let mut parms: Vec<(i64, Field)> = param_rows
                    .iter()
                    .map(|p| {
                        (
                            dv_i64(&p[4]),
                            Field {
                                name: dv_str(&p[0]),
                                field_type: dv_str(&p[1]),
                                required: matches!(&p[2], cozo::DataValue::Bool(true)),
                                description: dv_str(&p[3]),
                            },
                        )
                    })
                    .collect();
                parms.sort_by_key(|(i, _)| *i);

                (
                    dv_i64(&r[3]),
                    Method {
                        name: mname,
                        description: dv_str(&r[1]),
                        parameters: parms.into_iter().map(|(_, p)| p).collect(),
                        return_type: dv_str(&r[2]),
                        file_path: None,
                        start_line: None,
                        end_line: None,
                    },
                )
            })
            .collect();
        indexed.sort_by_key(|(i, _)| *i);
        indexed.into_iter().map(|(_, m)| m).collect()
    }

    fn query_ownership(
        &self,
        ws: &str,
        ctx: &str,
        owner_kind: &str,
        owner: &str,
        state: &str,
    ) -> Ownership {
        let rows = self
            .run_script(
                "?[team, owners_json, rationale] := *owner_meta{workspace: $ws, context: $ctx, owner_kind: $ok, owner: $owner, state: $st, team, owners_json, rationale @ 'NOW'}",
                params_map(&[("ws", ws), ("ctx", ctx), ("ok", owner_kind), ("owner", owner), ("st", state)]),
                ScriptMutability::Immutable,
            )
            .map(|r| r.rows)
            .unwrap_or_default();

        if let Some(row) = rows.first() {
            let owners = serde_json::from_str::<Vec<String>>(&dv_str(&row[1])).unwrap_or_default();
            Ownership {
                team: dv_str(&row[0]),
                owners,
                rationale: dv_str(&row[2]),
            }
        } else {
            Ownership::default()
        }
    }

    fn query_indexed_strings(
        &self,
        query: &str,
        params: BTreeMap<String, cozo::DataValue>,
    ) -> Vec<String> {
        let rows = self
            .run_script(query, params, ScriptMutability::Immutable)
            .map(|r| r.rows)
            .unwrap_or_default();

        let mut indexed: Vec<(i64, String)> = rows
            .iter()
            .map(|row| (dv_i64(&row[0]), dv_str(&row[1])))
            .collect();
        indexed.sort_by_key(|(idx, _)| *idx);
        indexed.into_iter().map(|(_, value)| value).collect()
    }

    fn policy_kind_key(kind: &PolicyKind) -> &'static str {
        match kind {
            PolicyKind::Domain => "domain",
            PolicyKind::ProcessManager => "process_manager",
            PolicyKind::Integration => "integration",
        }
    }

    /// Query invariants for an entity, ordered by idx.
    fn query_invariants(&self, ws: &str, ctx: &str, entity: &str, state: &str) -> Vec<String> {
        let params = params_map(&[("ws", ws), ("ctx", ctx), ("ent", entity), ("st", state)]);
        let rows = self
            .run_script(
                "?[idx, text] := \
                    *invariant{workspace: $ws, context: $ctx, entity: $ent, \
                               idx, state: $st, text @ 'NOW'}",
                params,
                ScriptMutability::Immutable,
            )
            .map(|r| r.rows)
            .unwrap_or_default();

        let mut indexed: Vec<(i64, String)> = rows
            .iter()
            .map(|r| (dv_i64(&r[0]), dv_str(&r[1])))
            .collect();
        indexed.sort_by_key(|(i, _)| *i);
        indexed.into_iter().map(|(_, t)| t).collect()
    }

    /// Query validation rules for a value object, ordered by idx.
    fn query_vo_rules(&self, ws: &str, ctx: &str, vo: &str, state: &str) -> Vec<String> {
        let params = params_map(&[("ws", ws), ("ctx", ctx), ("vo", vo), ("st", state)]);
        let rows = self
            .run_script(
                "?[idx, text] := \
                    *vo_rule{workspace: $ws, context: $ctx, value_object: $vo, \
                             idx, state: $st, text @ 'NOW'}",
                params,
                ScriptMutability::Immutable,
            )
            .map(|r| r.rows)
            .unwrap_or_default();

        let mut indexed: Vec<(i64, String)> = rows
            .iter()
            .map(|r| (dv_i64(&r[0]), dv_str(&r[1])))
            .collect();
        indexed.sort_by_key(|(i, _)| *i);
        indexed.into_iter().map(|(_, t)| t).collect()
    }

    // ── Private: State Decomposition ───────────────────────────────────────

    /// Decompose a DomainModel into relational rows tagged with `state`.
    fn save_state(&self, workspace: &str, model: &DomainModel, state: &str) -> Result<()> {
        self.save_project_metadata(workspace, model)?;
        self.clear_state(workspace, state)?;
        self.save_owner_meta(
            workspace,
            "",
            "project",
            &model.name,
            &model.ownership,
            state,
        )?;

        for bc in &model.bounded_contexts {
            let params = params_map(&[
                ("ws", workspace),
                ("name", &bc.name),
                ("st", state),
                ("desc", &bc.description),
                ("mp", &bc.module_path),
            ]);
            self.run_script(
                "?[workspace, name, state, description, module_path] <- [[$ws, $name, $st, $desc, $mp]] :put context { workspace, name, state => description, module_path }",
                params,
                ScriptMutability::Mutable,
            ).map_err(|e| anyhow::anyhow!("save context '{}': {:?}", bc.name, e))?;

            self.save_owner_meta(
                workspace,
                &bc.name,
                "context",
                &bc.name,
                &bc.ownership,
                state,
            )?;

            for dep in &bc.dependencies {
                self.run_script(
                    "?[workspace, from_ctx, to_ctx, state] <- [[$ws, $from, $to, $st]] :put context_dep { workspace, from_ctx, to_ctx, state }",
                    params_map(&[("ws", workspace), ("from", &bc.name), ("to", dep), ("st", state)]),
                    ScriptMutability::Mutable,
                ).map_err(|e| anyhow::anyhow!("save context_dep: {:?}", e))?;
            }

            for aggregate in &bc.aggregates {
                self.run_script(
                    "?[workspace, context, name, state, description, root_entity] <- [[$ws, $ctx, $name, $st, $desc, $root]] :put aggregate { workspace, context, name, state => description, root_entity }",
                    params_map(&[("ws", workspace), ("ctx", &bc.name), ("name", &aggregate.name), ("st", state), ("desc", &aggregate.description), ("root", &aggregate.root_entity)]),
                    ScriptMutability::Mutable,
                ).map_err(|e| anyhow::anyhow!("save aggregate '{}': {:?}", aggregate.name, e))?;
                self.save_owner_meta(
                    workspace,
                    &bc.name,
                    "aggregate",
                    &aggregate.name,
                    &aggregate.ownership,
                    state,
                )?;
                for entity in &aggregate.entities {
                    self.run_script(
                        "?[workspace, context, aggregate, member_kind, member, state] <- [[$ws, $ctx, $agg, 'entity', $member, $st]] :put aggregate_member { workspace, context, aggregate, member_kind, member, state }",
                        params_map(&[("ws", workspace), ("ctx", &bc.name), ("agg", &aggregate.name), ("member", entity), ("st", state)]),
                        ScriptMutability::Mutable,
                    ).map_err(|e| anyhow::anyhow!("save aggregate entity member: {:?}", e))?;
                }
                for value_object in &aggregate.value_objects {
                    self.run_script(
                        "?[workspace, context, aggregate, member_kind, member, state] <- [[$ws, $ctx, $agg, 'value_object', $member, $st]] :put aggregate_member { workspace, context, aggregate, member_kind, member, state }",
                        params_map(&[("ws", workspace), ("ctx", &bc.name), ("agg", &aggregate.name), ("member", value_object), ("st", state)]),
                        ScriptMutability::Mutable,
                    ).map_err(|e| anyhow::anyhow!("save aggregate value_object member: {:?}", e))?;
                }
            }

            for entity in &bc.entities {
                let mut params = params_map(&[
                    ("ws", workspace),
                    ("ctx", &bc.name),
                    ("name", &entity.name),
                    ("st", state),
                    ("desc", &entity.description),
                ]);
                params.insert("agg".into(), cozo::DataValue::Bool(entity.aggregate_root));
                params.insert(
                    "file".into(),
                    cozo::DataValue::Str(entity.file_path.as_deref().unwrap_or("").into()),
                );
                params.insert(
                    "sl".into(),
                    int_dv(usize_to_i64(entity.start_line.unwrap_or(0))),
                );
                params.insert(
                    "el".into(),
                    int_dv(usize_to_i64(entity.end_line.unwrap_or(0))),
                );
                self.run_script(
                    "?[workspace, context, name, state, description, aggregate_root, file_path, start_line, end_line] <- [[$ws, $ctx, $name, $st, $desc, $agg, $file, $sl, $el]] :put entity { workspace, context, name, state => description, aggregate_root, file_path, start_line, end_line }",
                    params,
                    ScriptMutability::Mutable,
                ).map_err(|e| anyhow::anyhow!("save entity '{}': {:?}", entity.name, e))?;
                self.save_fields(
                    workspace,
                    &bc.name,
                    "entity",
                    &entity.name,
                    &entity.fields,
                    state,
                )?;
                self.save_methods(
                    workspace,
                    &bc.name,
                    "entity",
                    &entity.name,
                    &entity.methods,
                    state,
                )?;
                for (idx, inv) in entity.invariants.iter().enumerate() {
                    let mut params = params_map(&[
                        ("ws", workspace),
                        ("ctx", &bc.name),
                        ("ent", &entity.name),
                        ("st", state),
                        ("text", inv),
                    ]);
                    params.insert("idx".into(), int_dv(usize_to_i64(idx)));
                    self.run_script(
                        "?[workspace, context, entity, idx, state, text] <- [[$ws, $ctx, $ent, $idx, $st, $text]] :put invariant { workspace, context, entity, idx, state => text }",
                        params,
                        ScriptMutability::Mutable,
                    ).map_err(|e| anyhow::anyhow!("save invariant: {:?}", e))?;
                }
            }

            for policy in &bc.policies {
                let kind_str = Self::policy_kind_key(&policy.kind).to_string();
                self.run_script(
                    "?[workspace, context, name, state, description, kind] <- [[$ws, $ctx, $name, $st, $desc, $kind]] :put policy { workspace, context, name, state => description, kind }",
                    params_map(&[("ws", workspace), ("ctx", &bc.name), ("name", &policy.name), ("st", state), ("desc", &policy.description), ("kind", &kind_str)]),
                    ScriptMutability::Mutable,
                ).map_err(|e| anyhow::anyhow!("save policy '{}': {:?}", policy.name, e))?;
                self.save_owner_meta(
                    workspace,
                    &bc.name,
                    "policy",
                    &policy.name,
                    &policy.ownership,
                    state,
                )?;
                for (idx, trigger) in policy.triggers.iter().enumerate() {
                    let mut params = params_map(&[
                        ("ws", workspace),
                        ("ctx", &bc.name),
                        ("policy", &policy.name),
                        ("link", trigger),
                        ("st", state),
                    ]);
                    params.insert("idx".into(), int_dv(usize_to_i64(idx)));
                    self.run_script(
                        "?[workspace, context, policy, link_kind, link, idx, state] <- [[$ws, $ctx, $policy, 'trigger', $link, $idx, $st]] :put policy_link { workspace, context, policy, link_kind, link, idx, state }",
                        params,
                        ScriptMutability::Mutable,
                    ).map_err(|e| anyhow::anyhow!("save policy trigger: {:?}", e))?;
                }
                for (idx, command) in policy.commands.iter().enumerate() {
                    let mut params = params_map(&[
                        ("ws", workspace),
                        ("ctx", &bc.name),
                        ("policy", &policy.name),
                        ("link", command),
                        ("st", state),
                    ]);
                    params.insert("idx".into(), int_dv(usize_to_i64(idx)));
                    self.run_script(
                        "?[workspace, context, policy, link_kind, link, idx, state] <- [[$ws, $ctx, $policy, 'command', $link, $idx, $st]] :put policy_link { workspace, context, policy, link_kind, link, idx, state }",
                        params,
                        ScriptMutability::Mutable,
                    ).map_err(|e| anyhow::anyhow!("save policy command: {:?}", e))?;
                }
            }

            for read_model in &bc.read_models {
                self.run_script(
                    "?[workspace, context, name, state, description, source] <- [[$ws, $ctx, $name, $st, $desc, $src]] :put read_model { workspace, context, name, state => description, source }",
                    params_map(&[("ws", workspace), ("ctx", &bc.name), ("name", &read_model.name), ("st", state), ("desc", &read_model.description), ("src", &read_model.source)]),
                    ScriptMutability::Mutable,
                ).map_err(|e| anyhow::anyhow!("save read_model '{}': {:?}", read_model.name, e))?;
                self.save_owner_meta(
                    workspace,
                    &bc.name,
                    "read_model",
                    &read_model.name,
                    &read_model.ownership,
                    state,
                )?;
                self.save_fields(
                    workspace,
                    &bc.name,
                    "read_model",
                    &read_model.name,
                    &read_model.fields,
                    state,
                )?;
            }

            for ep in &bc.api_endpoints {
                let params = params_map(&[
                    ("ws", workspace),
                    ("ctx", &bc.name),
                    ("id", &ep.id),
                    ("st", state),
                    ("svc", &ep.service_id),
                    ("met", &ep.method),
                    ("path", &ep.route_pattern),
                    ("desc", &ep.description),
                ]);
                self.run_script(
                    "?[workspace, context, id, state, service_id, method, route_pattern, description] <- \
                     [[$ws, $ctx, $id, $st, $svc, $met, $path, $desc]] \
                     :put api_endpoint { workspace, context, id, state => service_id, method, route_pattern, description }",
                    params,
                    ScriptMutability::Mutable,
                ).map_err(|e| anyhow::anyhow!("save api_endpoint: {:?}", e))?;
            }
            for svc in &bc.services {
                let kind_str = format!("{:?}", svc.kind).to_lowercase();
                let mut params = params_map(&[
                    ("ws", workspace),
                    ("ctx", &bc.name),
                    ("name", &svc.name),
                    ("st", state),
                    ("desc", &svc.description),
                    ("kind", &kind_str),
                ]);
                params.insert(
                    "file".into(),
                    cozo::DataValue::Str(svc.file_path.as_deref().unwrap_or("").into()),
                );
                params.insert(
                    "sl".into(),
                    int_dv(usize_to_i64(svc.start_line.unwrap_or(0))),
                );
                params.insert("el".into(), int_dv(usize_to_i64(svc.end_line.unwrap_or(0))));
                self.run_script(
                    "?[workspace, context, name, state, description, kind, file_path, start_line, end_line] <- [[$ws, $ctx, $name, $st, $desc, $kind, $file, $sl, $el]] :put service { workspace, context, name, state => description, kind, file_path, start_line, end_line }",
                    params,
                    ScriptMutability::Mutable,
                ).map_err(|e| anyhow::anyhow!("save service '{}': {:?}", svc.name, e))?;
                self.save_methods(
                    workspace,
                    &bc.name,
                    "service",
                    &svc.name,
                    &svc.methods,
                    state,
                )?;
                for dep in &svc.dependencies {
                    self.run_script(
                        "?[workspace, context, service, dep, state] <- [[$ws, $ctx, $svc, $dep, $st]] :put service_dep { workspace, context, service, dep, state }",
                        params_map(&[("ws", workspace), ("ctx", &bc.name), ("svc", &svc.name), ("dep", dep), ("st", state)]),
                        ScriptMutability::Mutable,
                    ).map_err(|e| anyhow::anyhow!("save service_dep: {:?}", e))?;
                }
            }

            for evt in &bc.events {
                let mut params = params_map(&[
                    ("ws", workspace),
                    ("ctx", &bc.name),
                    ("name", &evt.name),
                    ("st", state),
                    ("desc", &evt.description),
                    ("src", &evt.source),
                ]);
                params.insert(
                    "file".into(),
                    cozo::DataValue::Str(evt.file_path.as_deref().unwrap_or("").into()),
                );
                params.insert(
                    "sl".into(),
                    int_dv(usize_to_i64(evt.start_line.unwrap_or(0))),
                );
                params.insert("el".into(), int_dv(usize_to_i64(evt.end_line.unwrap_or(0))));
                self.run_script(
                    "?[workspace, context, name, state, description, source, file_path, start_line, end_line] <- [[$ws, $ctx, $name, $st, $desc, $src, $file, $sl, $el]] :put event { workspace, context, name, state => description, source, file_path, start_line, end_line }",
                    params,
                    ScriptMutability::Mutable,
                ).map_err(|e| anyhow::anyhow!("save event '{}': {:?}", evt.name, e))?;
                self.save_fields(workspace, &bc.name, "event", &evt.name, &evt.fields, state)?;
            }

            for vo in &bc.value_objects {
                let mut params = params_map(&[
                    ("ws", workspace),
                    ("ctx", &bc.name),
                    ("name", &vo.name),
                    ("st", state),
                    ("desc", &vo.description),
                ]);
                params.insert(
                    "file".into(),
                    cozo::DataValue::Str(vo.file_path.as_deref().unwrap_or("").into()),
                );
                params.insert(
                    "sl".into(),
                    int_dv(usize_to_i64(vo.start_line.unwrap_or(0))),
                );
                params.insert("el".into(), int_dv(usize_to_i64(vo.end_line.unwrap_or(0))));
                self.run_script(
                    "?[workspace, context, name, state, description, file_path, start_line, end_line] <- [[$ws, $ctx, $name, $st, $desc, $file, $sl, $el]] :put value_object { workspace, context, name, state => description, file_path, start_line, end_line }",
                    params,
                    ScriptMutability::Mutable,
                ).map_err(|e| anyhow::anyhow!("save value_object '{}': {:?}", vo.name, e))?;
                self.save_fields(
                    workspace,
                    &bc.name,
                    "value_object",
                    &vo.name,
                    &vo.fields,
                    state,
                )?;
                for (idx, rule) in vo.validation_rules.iter().enumerate() {
                    let mut p = params_map(&[
                        ("ws", workspace),
                        ("ctx", &bc.name),
                        ("vo", &vo.name),
                        ("st", state),
                        ("text", rule),
                    ]);
                    p.insert("idx".into(), int_dv(usize_to_i64(idx)));
                    self.run_script(
                        "?[workspace, context, value_object, idx, state, text] <- [[$ws, $ctx, $vo, $idx, $st, $text]] :put vo_rule { workspace, context, value_object, idx, state => text }",
                        p,
                        ScriptMutability::Mutable,
                    ).map_err(|e| anyhow::anyhow!("save vo_rule: {:?}", e))?;
                }
            }

            for repo in &bc.repositories {
                let mut params = params_map(&[
                    ("ws", workspace),
                    ("ctx", &bc.name),
                    ("name", &repo.name),
                    ("st", state),
                    ("agg", &repo.aggregate),
                ]);
                params.insert(
                    "file".into(),
                    cozo::DataValue::Str(repo.file_path.as_deref().unwrap_or("").into()),
                );
                params.insert(
                    "sl".into(),
                    int_dv(usize_to_i64(repo.start_line.unwrap_or(0))),
                );
                params.insert(
                    "el".into(),
                    int_dv(usize_to_i64(repo.end_line.unwrap_or(0))),
                );
                self.run_script(
                    "?[workspace, context, name, state, aggregate, file_path, start_line, end_line] <- [[$ws, $ctx, $name, $st, $agg, $file, $sl, $el]] :put repository { workspace, context, name, state => aggregate, file_path, start_line, end_line }",
                    params,
                    ScriptMutability::Mutable,
                ).map_err(|e| anyhow::anyhow!("save repository '{}': {:?}", repo.name, e))?;
                self.save_methods(
                    workspace,
                    &bc.name,
                    "repository",
                    &repo.name,
                    &repo.methods,
                    state,
                )?;
            }

            for module in &bc.modules {
                let mut params = params_map(&[
                    ("ws", workspace),
                    ("ctx", &bc.name),
                    ("name", &module.name),
                    ("st", state),
                    ("path", &module.path),
                    ("fp", &module.file_path),
                    ("desc", &module.description),
                ]);
                params.insert("public".into(), cozo::DataValue::Bool(module.public));
                self.run_script(
                    "?[workspace, context, name, state, path, public, file_path, description] <- [[$ws, $ctx, $name, $st, $path, $public, $fp, $desc]] :put module { workspace, context, name, state => path, public, file_path, description }",
                    params,
                    ScriptMutability::Mutable,
                ).map_err(|e| anyhow::anyhow!("save module '{}': {:?}", module.name, e))?;
            }
        }

        for system in &model.external_systems {
            self.run_script(
                "?[workspace, name, state, description, kind, rationale] <- [[$ws, $name, $st, $desc, $kind, $rationale]] :put external_system { workspace, name, state => description, kind, rationale }",
                params_map(&[("ws", workspace), ("name", &system.name), ("st", state), ("desc", &system.description), ("kind", &system.kind), ("rationale", &system.rationale)]),
                ScriptMutability::Mutable,
            ).map_err(|e| anyhow::anyhow!("save external_system '{}': {:?}", system.name, e))?;
            self.save_owner_meta(
                workspace,
                "",
                "external_system",
                &system.name,
                &system.ownership,
                state,
            )?;
            for (idx, ctx) in system.consumed_by_contexts.iter().enumerate() {
                let mut params = params_map(&[
                    ("ws", workspace),
                    ("name", &system.name),
                    ("ctx", ctx),
                    ("st", state),
                ]);
                params.insert("idx".into(), int_dv(usize_to_i64(idx)));
                self.run_script(
                    "?[workspace, system, context, idx, state] <- [[$ws, $name, $ctx, $idx, $st]] :put external_system_context { workspace, system, context, idx, state }",
                    params,
                    ScriptMutability::Mutable,
                ).map_err(|e| anyhow::anyhow!("save external_system_context: {:?}", e))?;
            }
        }

        for decision in &model.architectural_decisions {
            let status = format!("{:?}", decision.status).to_lowercase();
            self.run_script(
                "?[workspace, id, state, title, status, scope, date, rationale] <- [[$ws, $id, $st, $title, $status, $scope, $date, $rationale]] :put architectural_decision { workspace, id, state => title, status, scope, date, rationale }",
                params_map(&[("ws", workspace), ("id", &decision.id), ("st", state), ("title", &decision.title), ("status", &status), ("scope", &decision.scope), ("date", &decision.date), ("rationale", &decision.rationale)]),
                ScriptMutability::Mutable,
            ).map_err(|e| anyhow::anyhow!("save architectural_decision '{}': {:?}", decision.id, e))?;
            self.save_owner_meta(
                workspace,
                "",
                "architectural_decision",
                &decision.id,
                &decision.ownership,
                state,
            )?;
            for (idx, ctx) in decision.contexts.iter().enumerate() {
                let mut params = params_map(&[
                    ("ws", workspace),
                    ("id", &decision.id),
                    ("ctx", ctx),
                    ("st", state),
                ]);
                params.insert("idx".into(), int_dv(usize_to_i64(idx)));
                self.run_script(
                    "?[workspace, decision_id, context, idx, state] <- [[$ws, $id, $ctx, $idx, $st]] :put decision_context { workspace, decision_id, context, idx, state }",
                    params,
                    ScriptMutability::Mutable,
                ).map_err(|e| anyhow::anyhow!("save decision_context: {:?}", e))?;
            }
            for (idx, consequence) in decision.consequences.iter().enumerate() {
                let mut params = params_map(&[
                    ("ws", workspace),
                    ("id", &decision.id),
                    ("text", consequence),
                    ("st", state),
                ]);
                params.insert("idx".into(), int_dv(usize_to_i64(idx)));
                self.run_script(
                    "?[workspace, decision_id, idx, state, text] <- [[$ws, $id, $idx, $st, $text]] :put decision_consequence { workspace, decision_id, idx, state => text }",
                    params,
                    ScriptMutability::Mutable,
                ).map_err(|e| anyhow::anyhow!("save decision_consequence: {:?}", e))?;
            }
        }

        // Batched bulk inserts for the high-volume flat relations. `sv` builds a
        // string `DataValue`; each relation sends all rows in one `:put`.
        let sv = |x: &str| cozo::DataValue::Str(x.into());

        // Save AST edges
        let ast_rows: Vec<cozo::DataValue> = model
            .ast_edges
            .iter()
            .map(|edge| {
                cozo::DataValue::List(vec![
                    sv(workspace),
                    sv(&edge.from_node),
                    sv(&edge.to_node),
                    sv(&edge.edge_type),
                    sv(state),
                    sv(&edge.file_path),
                    cozo::DataValue::from(usize_to_i64(edge.line)),
                ])
            })
            .collect();
        self.batch_put(
            ast_rows,
            "?[workspace, from_node, to_node, edge_type, state, file_path, line] <- $rows \
             :put ast_edge { workspace, from_node, to_node, edge_type, state => file_path, line }",
            "save ast_edges",
        )?;

        // Save source files
        for sf in &model.source_files {
            self.run_script(
                "?[workspace, path, state, context, language] <- [[$ws, $path, $st, $ctx, $lang]] \
                 :put source_file { workspace, path, state => context, language }",
                params_map(&[
                    ("ws", workspace),
                    ("path", &sf.path),
                    ("st", state),
                    ("ctx", &sf.context),
                    ("lang", &sf.language),
                ]),
                ScriptMutability::Mutable,
            )
            .map_err(|e| anyhow::anyhow!("save source_file '{}': {:?}", sf.path, e))?;
        }

        // Save symbols
        let sym_rows: Vec<cozo::DataValue> = model
            .symbols
            .iter()
            .map(|sym| {
                cozo::DataValue::List(vec![
                    sv(workspace),
                    sv(&sym.name),
                    sv(state),
                    sv(&sym.kind),
                    sv(&sym.context),
                    sv(&sym.file_path),
                    int_dv(usize_to_i64(sym.start_line)),
                    int_dv(usize_to_i64(sym.end_line)),
                    sv(&sym.visibility),
                ])
            })
            .collect();
        self.batch_put(
            sym_rows,
            "?[workspace, name, state, kind, context, file_path, start_line, end_line, visibility] <- $rows \
             :put symbol { workspace, name, state => kind, context, file_path, start_line, end_line, visibility }",
            "save symbols",
        )?;

        // Save import edges
        let import_rows: Vec<cozo::DataValue> = model
            .import_edges
            .iter()
            .map(|ie| {
                cozo::DataValue::List(vec![
                    sv(workspace),
                    sv(&ie.from_file),
                    sv(&ie.to_module),
                    sv(state),
                    sv(&ie.context),
                ])
            })
            .collect();
        self.batch_put(
            import_rows,
            "?[workspace, from_file, to_module, state, context] <- $rows \
             :put import_edge { workspace, from_file, to_module, state => context }",
            "save import_edges",
        )?;

        // Save reference edges
        let reference_rows: Vec<cozo::DataValue> = model
            .reference_edges
            .iter()
            .map(|re| {
                cozo::DataValue::List(vec![
                    sv(workspace),
                    sv(&re.from_file),
                    sv(&re.to_path),
                    sv(&re.reference_kind),
                    int_dv(usize_to_i64(re.line)),
                    sv(state),
                    sv(&re.context),
                ])
            })
            .collect();
        self.batch_put(
            reference_rows,
            "?[workspace, from_file, to_path, reference_kind, line, state, context] <- $rows \
             :put reference_edge { workspace, from_file, to_path, reference_kind, line, state => context }",
            "save reference_edges",
        )?;

        // Save call edges
        let call_rows: Vec<cozo::DataValue> = model
            .call_edges
            .iter()
            .map(|ce| {
                cozo::DataValue::List(vec![
                    sv(workspace),
                    sv(&ce.caller),
                    sv(&ce.callee),
                    sv(state),
                    sv(&ce.file_path),
                    int_dv(usize_to_i64(ce.line)),
                    sv(&ce.context),
                ])
            })
            .collect();
        self.batch_put(
            call_rows,
            "?[workspace, caller, callee, state, file_path, line, context] <- $rows \
             :put calls_symbol { workspace, caller, callee, state => file_path, line, context }",
            "save call_edges",
        )?;

        self.apply_inferred_layers(workspace, model)?;
        self.record_snapshot(workspace, state)?;
        self.invalidate_reasoning_claims_for_dependency(workspace, state)?;

        Ok(())
    }

    fn record_snapshot(&self, workspace: &str, state: &str) -> Result<i64> {
        let now_us = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros();
        let now_us = u128_to_i64_saturating(now_us);
        let latest_ts = self
            .list_snapshots(workspace, state)?
            .into_iter()
            .next()
            .unwrap_or(0);
        let ts_us = now_us.max(latest_ts.saturating_add(1));
        let mut snap_params = params_map(&[("ws", workspace), ("st", state)]);
        snap_params.insert("ts".into(), int_dv(ts_us));
        self.run_mutation_script(
            "?[workspace, state, timestamp_us] <- [[$ws, $st, $ts]] \
             :put snapshot_log { workspace, state, timestamp_us }",
            snap_params,
            format!("save snapshot_log for '{workspace}' state '{state}'"),
        )?;
        Ok(ts_us)
    }

    /// Retract all current rows for a workspace+state (preserves temporal history).
    ///
    /// Instead of `:rm` (which destroys history), this creates RETRACT entries
    /// so that point-in-time queries at earlier timestamps still return old data.
    fn clear_state(&self, workspace: &str, state: &str) -> Result<()> {
        let params = params_map(&[("ws", workspace), ("st", state)]);
        // Each table: query current rows via @ 'NOW', then :put with vld='RETRACT'
        // Value columns use defaults (irrelevant for retraction semantics).
        let tables = [
            ("owner_meta", "workspace, context, owner_kind, owner, state"),
            ("context", "workspace, name, state"),
            ("context_dep", "workspace, from_ctx, to_ctx, state"),
            ("aggregate", "workspace, context, name, state"),
            (
                "aggregate_member",
                "workspace, context, aggregate, member_kind, member, state",
            ),
            ("entity", "workspace, context, name, state"),
            ("policy", "workspace, context, name, state"),
            (
                "policy_link",
                "workspace, context, policy, link_kind, link, idx, state",
            ),
            ("read_model", "workspace, context, name, state"),
            ("service", "workspace, context, name, state"),
            ("service_dep", "workspace, context, service, dep, state"),
            ("event", "workspace, context, name, state"),
            ("value_object", "workspace, context, name, state"),
            ("repository", "workspace, context, name, state"),
            ("module", "workspace, context, name, state"),
            ("api_endpoint", "workspace, context, id, state"),
            (
                "invokes_endpoint",
                "workspace, caller_context, caller_method, endpoint_id, state",
            ),
            (
                "calls_external_system",
                "workspace, caller_context, caller_method, ext_id, state",
            ),
            ("external_system", "workspace, name, state"),
            (
                "external_system_context",
                "workspace, system, context, idx, state",
            ),
            ("architectural_decision", "workspace, id, state"),
            (
                "decision_context",
                "workspace, decision_id, context, idx, state",
            ),
            ("decision_consequence", "workspace, decision_id, idx, state"),
            (
                "field",
                "workspace, context, owner_kind, owner, name, state",
            ),
            (
                "method",
                "workspace, context, owner_kind, owner, name, state",
            ),
            (
                "method_param",
                "workspace, context, owner_kind, owner, method, name, state",
            ),
            (
                "ast_edge",
                "workspace, state, from_node, to_node, edge_type",
            ),
            ("source_file", "workspace, path, state"),
            ("symbol", "workspace, name, state"),
            ("import_edge", "workspace, from_file, to_module, state"),
            (
                "reference_edge",
                "workspace, from_file, to_path, reference_kind, line, state",
            ),
            ("calls_symbol", "workspace, caller, callee, state"),
        ];
        for (rel, keys) in tables {
            let script = format!(
                "?[{keys}, vld] := *{rel}{{{keys} @ 'NOW'}}, workspace = $ws, state = $st, vld = 'RETRACT' \
                 :put {rel} {{{keys}, vld}}"
            );
            self.run_mutation_script(
                &script,
                params.clone(),
                format!("clear_state retract {rel} for '{state}'"),
            )?;
        }
        self.run_mutation_script(
            "?[workspace, context, entity, idx, state, text, vld] := *invariant{workspace, context, entity, idx, state, text @ 'NOW'}, workspace = $ws, state = $st, vld = 'RETRACT' :put invariant { workspace, context, entity, idx, state, vld => text }",
            params.clone(),
            format!("clear_state retract invariant for '{state}'"),
        )?;
        self.run_mutation_script(
            "?[workspace, context, value_object, idx, state, text, vld] := *vo_rule{workspace, context, value_object, idx, state, text @ 'NOW'}, workspace = $ws, state = $st, vld = 'RETRACT' :put vo_rule { workspace, context, value_object, idx, state, vld => text }",
            params,
            format!("clear_state retract vo_rule for '{state}'"),
        )?;
        Ok(())
    }

    /// Reconstruct a DomainModel from relational rows for a given state.
    fn reconstruct_model(&self, workspace_path: &str, state: &str) -> Result<Option<DomainModel>> {
        let ws = canonicalize_path(workspace_path);
        let p = params_map(&[("ws", &ws), ("st", state)]);

        // Project metadata
        let proj = self
            .run_script(
                "?[name, description, rules_json, tech_stack_json, conventions_json] := \
                    *project{workspace: $ws, name, description, rules_json, tech_stack_json, conventions_json}",
                params_map(&[("ws", &ws)]),
                ScriptMutability::Immutable,
            )
            .ok();

        // Contexts for this state
        let ctxs = self
            .run_script(
                "?[name, description, module_path] := \
                    *context{workspace: $ws, name, state: $st, description, module_path @ 'NOW'}",
                p.clone(),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("reconstruct contexts: {:?}", e))?;

        let project_row = proj.as_ref().and_then(|rows| rows.rows.first());
        let has_project = project_row.is_some();

        if ctxs.rows.is_empty() && !has_project {
            return Ok(None);
        }

        // Extract project-level metadata
        let (project_name, description, rules, tech_stack, conventions) = if let Some(r) =
            project_row
        {
            (
                dv_str(&r[0]),
                dv_str(&r[1]),
                serde_json::from_str::<Vec<ArchitecturalRule>>(&dv_str(&r[2])).unwrap_or_default(),
                serde_json::from_str::<TechStack>(&dv_str(&r[3])).unwrap_or_default(),
                serde_json::from_str::<Conventions>(&dv_str(&r[4])).unwrap_or_default(),
            )
        } else {
            let name = std::path::Path::new(workspace_path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "Unnamed".into());
            (
                name,
                String::new(),
                vec![],
                TechStack::default(),
                Conventions::default(),
            )
        };

        let project_ownership = self.query_ownership(&ws, "", "project", &project_name, state);

        // Reconstruct each bounded context
        let mut bounded_contexts = Vec::new();
        for row in &ctxs.rows {
            let ctx_name = dv_str(&row[0]);

            // Dependencies
            let deps = self
                .run_script(
                    "?[to_ctx] := *context_dep{workspace: $ws, from_ctx: $ctx, to_ctx, state: $st @ 'NOW'}",
                    params_map(&[("ws", &ws), ("ctx", &ctx_name), ("st", state)]),
                    ScriptMutability::Immutable,
                )
                .map(|r| r.rows)
                .unwrap_or_default();
            let dependencies: Vec<String> = deps.iter().map(|r| dv_str(&r[0])).collect();

            let ownership = self.query_ownership(&ws, &ctx_name, "context", &ctx_name, state);

            let aggs = self
                .run_script(
                    "?[name, description, root_entity] := *aggregate{workspace: $ws, context: $ctx, name, state: $st, description, root_entity @ 'NOW'}",
                    params_map(&[("ws", &ws), ("ctx", &ctx_name), ("st", state)]),
                    ScriptMutability::Immutable,
                )
                .map(|r| r.rows)
                .unwrap_or_default();
            let aggregates: Vec<Aggregate> = aggs
                .iter()
                .map(|r| {
                    let aggregate_name = dv_str(&r[0]);
                    let members = self
                        .run_script(
                            "?[member_kind, member] := *aggregate_member{workspace: $ws, context: $ctx, aggregate: $agg, member_kind, member, state: $st @ 'NOW'}",
                            params_map(&[("ws", &ws), ("ctx", &ctx_name), ("agg", &aggregate_name), ("st", state)]),
                            ScriptMutability::Immutable,
                        )
                        .map(|r| r.rows)
                        .unwrap_or_default();
                    Aggregate {
                        name: aggregate_name.clone(),
                        description: dv_str(&r[1]),
                        root_entity: dv_str(&r[2]),
                        entities: members.iter().filter(|m| dv_str(&m[0]) == "entity").map(|m| dv_str(&m[1])).collect(),
                        value_objects: members.iter().filter(|m| dv_str(&m[0]) == "value_object").map(|m| dv_str(&m[1])).collect(),
                        ownership: self.query_ownership(&ws, &ctx_name, "aggregate", &aggregate_name, state),
                    }
                })
                .collect();

            // Entities
            let ents = self
                .run_script(
                    "?[name, description, aggregate_root, file_path, start_line, end_line] := \
                        *entity{workspace: $ws, context: $ctx, name, state: $st, \
                                description, aggregate_root, file_path, start_line, end_line @ 'NOW'}",
                    params_map(&[("ws", &ws), ("ctx", &ctx_name), ("st", state)]),
                    ScriptMutability::Immutable,
                )
                .map(|r| r.rows)
                .unwrap_or_default();
            let entities: Vec<Entity> = ents
                .iter()
                .map(|r| {
                    let ename = dv_str(&r[0]);
                    Entity {
                        name: ename.clone(),
                        description: dv_str(&r[1]),
                        aggregate_root: matches!(&r[2], cozo::DataValue::Bool(true)),
                        fields: self.query_fields(&ws, &ctx_name, "entity", &ename, state),
                        methods: self.query_methods(&ws, &ctx_name, "entity", &ename, state),
                        invariants: self.query_invariants(&ws, &ctx_name, &ename, state),
                        file_path: dv_opt_string(&r[3]),
                        start_line: dv_opt_usize(&r[4]),
                        end_line: dv_opt_usize(&r[5]),
                    }
                })
                .collect();

            let policy_rows = self
                .run_script(
                    "?[name, description, kind] := *policy{workspace: $ws, context: $ctx, name, state: $st, description, kind @ 'NOW'}",
                    params_map(&[("ws", &ws), ("ctx", &ctx_name), ("st", state)]),
                    ScriptMutability::Immutable,
                )
                .map(|r| r.rows)
                .unwrap_or_default();
            let policies: Vec<Policy> = policy_rows
                .iter()
                .map(|r| {
                    let policy_name = dv_str(&r[0]);
                    let links = self
                        .run_script(
                            "?[idx, link_kind, link] := *policy_link{workspace: $ws, context: $ctx, policy: $policy, idx, state: $st, link_kind, link @ 'NOW'}",
                            params_map(&[("ws", &ws), ("ctx", &ctx_name), ("policy", &policy_name), ("st", state)]),
                            ScriptMutability::Immutable,
                        )
                        .map(|r| r.rows)
                        .unwrap_or_default();
                    let mut indexed = links.iter().map(|row| (dv_i64(&row[0]), dv_str(&row[1]), dv_str(&row[2]))).collect::<Vec<_>>();
                    indexed.sort_by_key(|(idx, _, _)| *idx);
                    Policy {
                        name: policy_name.clone(),
                        description: dv_str(&r[1]),
                        kind: match dv_str(&r[2]).as_str() {
                            "process_manager" => PolicyKind::ProcessManager,
                            "integration" => PolicyKind::Integration,
                            _ => PolicyKind::Domain,
                        },
                        triggers: indexed.iter().filter(|(_, kind, _)| kind == "trigger").map(|(_, _, link)| link.clone()).collect(),
                        commands: indexed.iter().filter(|(_, kind, _)| kind == "command").map(|(_, _, link)| link.clone()).collect(),
                        ownership: self.query_ownership(&ws, &ctx_name, "policy", &policy_name, state),
                    }
                })
                .collect();

            let read_model_rows = self
                .run_script(
                    "?[name, description, source] := *read_model{workspace: $ws, context: $ctx, name, state: $st, description, source @ 'NOW'}",
                    params_map(&[("ws", &ws), ("ctx", &ctx_name), ("st", state)]),
                    ScriptMutability::Immutable,
                )
                .map(|r| r.rows)
                .unwrap_or_default();
            let read_models: Vec<ReadModel> = read_model_rows
                .iter()
                .map(|r| {
                    let read_name = dv_str(&r[0]);
                    ReadModel {
                        name: read_name.clone(),
                        description: dv_str(&r[1]),
                        source: dv_str(&r[2]),
                        fields: self.query_fields(&ws, &ctx_name, "read_model", &read_name, state),
                        ownership: self.query_ownership(
                            &ws,
                            &ctx_name,
                            "read_model",
                            &read_name,
                            state,
                        ),
                    }
                })
                .collect();

            // Services
            let svcs = self
                .run_script(
                    "?[name, description, kind, file_path, start_line, end_line] := \
                        *service{workspace: $ws, context: $ctx, name, state: $st, \
                                 description, kind, file_path, start_line, end_line @ 'NOW'}",
                    params_map(&[("ws", &ws), ("ctx", &ctx_name), ("st", state)]),
                    ScriptMutability::Immutable,
                )
                .map(|r| r.rows)
                .unwrap_or_default();
            let services: Vec<Service> = svcs
                .iter()
                .map(|r| {
                    let svc_name = dv_str(&r[0]);
                    let svc_deps = self
                        .run_script(
                            "?[dep] := *service_dep{workspace: $ws, context: $ctx, service: $svc, dep, state: $st @ 'NOW'}",
                            params_map(&[
                                ("ws", &ws),
                                ("ctx", &ctx_name),
                                ("svc", &svc_name),
                                ("st", state),
                            ]),
                            ScriptMutability::Immutable,
                        )
                        .map(|r| r.rows)
                        .unwrap_or_default();
                    Service {
                        name: svc_name.clone(),
                        description: dv_str(&r[1]),
                        kind: match dv_str(&r[2]).as_str() {
                            "application" => ServiceKind::Application,
                            "infrastructure" => ServiceKind::Infrastructure,
                            _ => ServiceKind::Domain,
                        },
                        methods: self.query_methods(&ws, &ctx_name, "service", &svc_name, state),
                        dependencies: svc_deps.iter().map(|r| dv_str(&r[0])).collect(),
                        file_path: dv_opt_string(&r[3]),
                        start_line: dv_opt_usize(&r[4]),
                        end_line: dv_opt_usize(&r[5]),
                    }
                })
                .collect();

            // Events
            let evts = self
                .run_script(
                    "?[name, description, source, file_path, start_line, end_line] := \
                        *event{workspace: $ws, context: $ctx, name, state: $st, \
                               description, source, file_path, start_line, end_line @ 'NOW'}",
                    params_map(&[("ws", &ws), ("ctx", &ctx_name), ("st", state)]),
                    ScriptMutability::Immutable,
                )
                .map(|r| r.rows)
                .unwrap_or_default();
            let api_endpoints_rows = self.run_script(
                "?[id, service_id, method, route_pattern, description] := *api_endpoint{workspace: $ws, context: $ctx, id, state: $st, service_id, method, route_pattern, description @ 'NOW'}",
                params_map(&[("ws", &ws), ("ctx", &ctx_name), ("st", state)]),
                ScriptMutability::Immutable,
            ).map(|r| r.rows).unwrap_or_default();
            let api_endpoints: Vec<APIEndpoint> = api_endpoints_rows
                .iter()
                .map(|r| APIEndpoint {
                    id: dv_str(&r[0]),
                    service_id: dv_str(&r[1]),
                    method: dv_str(&r[2]),
                    route_pattern: dv_str(&r[3]),
                    description: dv_str(&r[4]),
                })
                .collect();

            let events: Vec<DomainEvent> = evts
                .iter()
                .map(|r| {
                    let ename = dv_str(&r[0]);
                    DomainEvent {
                        name: ename.clone(),
                        description: dv_str(&r[1]),
                        source: dv_str(&r[2]),
                        fields: self.query_fields(&ws, &ctx_name, "event", &ename, state),
                        file_path: dv_opt_string(&r[3]),
                        start_line: dv_opt_usize(&r[4]),
                        end_line: dv_opt_usize(&r[5]),
                    }
                })
                .collect();

            // Value objects
            let vos = self
                .run_script(
                    "?[name, description, file_path, start_line, end_line] := \
                        *value_object{workspace: $ws, context: $ctx, name, state: $st, description, file_path, start_line, end_line @ 'NOW'}",
                    params_map(&[("ws", &ws), ("ctx", &ctx_name), ("st", state)]),
                    ScriptMutability::Immutable,
                )
                .map(|r| r.rows)
                .unwrap_or_default();
            let value_objects: Vec<ValueObject> = vos
                .iter()
                .map(|r| {
                    let voname = dv_str(&r[0]);
                    ValueObject {
                        name: voname.clone(),
                        description: dv_str(&r[1]),
                        fields: self.query_fields(&ws, &ctx_name, "value_object", &voname, state),
                        validation_rules: self.query_vo_rules(&ws, &ctx_name, &voname, state),
                        file_path: dv_opt_string(&r[2]),
                        start_line: dv_opt_usize(&r[3]),
                        end_line: dv_opt_usize(&r[4]),
                    }
                })
                .collect();

            // Repositories
            let repos = self
                .run_script(
                    "?[name, aggregate, file_path, start_line, end_line] := \
                        *repository{workspace: $ws, context: $ctx, name, state: $st, aggregate, file_path, start_line, end_line @ 'NOW'}",
                    params_map(&[("ws", &ws), ("ctx", &ctx_name), ("st", state)]),
                    ScriptMutability::Immutable,
                )
                .map(|r| r.rows)
                .unwrap_or_default();
            let repositories: Vec<Repository> = repos
                .iter()
                .map(|r| {
                    let rname = dv_str(&r[0]);
                    Repository {
                        name: rname.clone(),
                        aggregate: dv_str(&r[1]),
                        methods: self.query_methods(&ws, &ctx_name, "repository", &rname, state),
                        file_path: dv_opt_string(&r[2]),
                        start_line: dv_opt_usize(&r[3]),
                        end_line: dv_opt_usize(&r[4]),
                    }
                })
                .collect();

            // Modules
            let mods = self
                .run_script(
                    "?[name, path, public, file_path, description] := \
                        *module{workspace: $ws, context: $ctx, name, state: $st, path, public, file_path, description @ 'NOW'}",
                    params_map(&[("ws", &ws), ("ctx", &ctx_name), ("st", state)]),
                    ScriptMutability::Immutable,
                )
                .map(|r| r.rows)
                .unwrap_or_default();
            let modules: Vec<Module> = mods
                .iter()
                .map(|r| Module {
                    name: dv_str(&r[0]),
                    path: dv_str(&r[1]),
                    public: r[2].get_bool().unwrap_or(false),
                    file_path: dv_str(&r[3]),
                    description: dv_str(&r[4]),
                })
                .collect();

            bounded_contexts.push(BoundedContext {
                name: ctx_name,
                description: dv_str(&row[1]),
                module_path: dv_str(&row[2]),
                ownership,
                aggregates,
                policies,
                read_models,
                entities,
                value_objects,
                services,
                api_endpoints,
                repositories,
                events,
                modules,
                dependencies,
            });
        }

        let external_system_rows = self
            .run_script(
                "?[name, description, kind, rationale] := *external_system{workspace: $ws, name, state: $st, description, kind, rationale @ 'NOW'}",
                params_map(&[("ws", &ws), ("st", state)]),
                ScriptMutability::Immutable,
            )
            .map(|r| r.rows)
            .unwrap_or_default();
        let external_systems: Vec<ExternalSystem> = external_system_rows
            .iter()
            .map(|r| {
                let system_name = dv_str(&r[0]);
                ExternalSystem {
                    name: system_name.clone(),
                    description: dv_str(&r[1]),
                    kind: dv_str(&r[2]),
                    consumed_by_contexts: self.query_indexed_strings(
                        "?[idx, context] := *external_system_context{workspace: $ws, system: $name, idx, state: $st, context @ 'NOW'}",
                        params_map(&[("ws", &ws), ("name", &system_name), ("st", state)]),
                    ),
                    rationale: dv_str(&r[3]),
                    ownership: self.query_ownership(&ws, "", "external_system", &system_name, state),
                }
            })
            .collect();

        let decision_rows = self
            .run_script(
                "?[id, title, status, scope, date, rationale] := *architectural_decision{workspace: $ws, id, state: $st, title, status, scope, date, rationale @ 'NOW'}",
                params_map(&[("ws", &ws), ("st", state)]),
                ScriptMutability::Immutable,
            )
            .map(|r| r.rows)
            .unwrap_or_default();
        let architectural_decisions: Vec<ArchitecturalDecision> = decision_rows
            .iter()
            .map(|r| {
                let decision_id = dv_str(&r[0]);
                ArchitecturalDecision {
                    id: decision_id.clone(),
                    title: dv_str(&r[1]),
                    status: match dv_str(&r[2]).as_str() {
                        "accepted" => DecisionStatus::Accepted,
                        "superseded" => DecisionStatus::Superseded,
                        "deprecated" => DecisionStatus::Deprecated,
                        _ => DecisionStatus::Proposed,
                    },
                    scope: dv_str(&r[3]),
                    date: dv_str(&r[4]),
                    rationale: dv_str(&r[5]),
                    consequences: self.query_indexed_strings(
                        "?[idx, text] := *decision_consequence{workspace: $ws, decision_id: $id, idx, state: $st, text @ 'NOW'}",
                        params_map(&[("ws", &ws), ("id", &decision_id), ("st", state)]),
                    ),
                    contexts: self.query_indexed_strings(
                        "?[idx, context] := *decision_context{workspace: $ws, decision_id: $id, idx, state: $st, context @ 'NOW'}",
                        params_map(&[("ws", &ws), ("id", &decision_id), ("st", state)]),
                    ),
                    ownership: self.query_ownership(&ws, "", "architectural_decision", &decision_id, state),
                }
            })
            .collect();

        Ok(Some(DomainModel {
            name: project_name,
            description,
            bounded_contexts,
            external_systems,
            architectural_decisions,
            ownership: project_ownership,
            rules,
            tech_stack,
            conventions,
            ast_edges: {
                let rows = self.run_script(
                    "?[from_node, to_node, edge_type, file_path, line] := *ast_edge{workspace: $ws, state: $st, from_node, to_node, edge_type, file_path, line @ 'NOW'}",
                    params_map(&[("ws", &ws), ("st", state)]),
                    ScriptMutability::Immutable,
                ).map(|r| r.rows).unwrap_or_default();
                rows.iter()
                    .map(|r| crate::domain::model::ASTEdge {
                        from_node: dv_str(&r[0]),
                        to_node: dv_str(&r[1]),
                        edge_type: dv_str(&r[2]),
                        file_path: dv_str(&r[3]),
                        line: i64_to_usize_saturating(dv_i64(&r[4]).max(0)),
                    })
                    .collect()
            },
            source_files: {
                let rows = self.run_script(
                    "?[path, context, language] := *source_file{workspace: $ws, path, state: $st, context, language @ 'NOW'}",
                    params_map(&[("ws", &ws), ("st", state)]),
                    ScriptMutability::Immutable,
                ).map(|r| r.rows).unwrap_or_default();
                rows.iter()
                    .map(|r| SourceFile {
                        path: dv_str(&r[0]),
                        context: dv_str(&r[1]),
                        language: dv_str(&r[2]),
                    })
                    .collect()
            },
            symbols: {
                let rows = self.run_script(
                    "?[name, kind, context, file_path, start_line, end_line, visibility] := \
                     *symbol{workspace: $ws, name, state: $st, kind, context, file_path, start_line, end_line, visibility @ 'NOW'}",
                    params_map(&[("ws", &ws), ("st", state)]),
                    ScriptMutability::Immutable,
                ).map(|r| r.rows).unwrap_or_default();
                rows.iter()
                    .map(|r| SymbolDef {
                        name: dv_str(&r[0]),
                        kind: dv_str(&r[1]),
                        context: dv_str(&r[2]),
                        file_path: dv_str(&r[3]),
                        start_line: i64_to_usize_saturating(dv_i64(&r[4])),
                        end_line: i64_to_usize_saturating(dv_i64(&r[5])),
                        visibility: dv_str(&r[6]),
                    })
                    .collect()
            },
            import_edges: {
                let rows = self.run_script(
                    "?[from_file, to_module, context] := *import_edge{workspace: $ws, from_file, to_module, state: $st, context @ 'NOW'}",
                    params_map(&[("ws", &ws), ("st", state)]),
                    ScriptMutability::Immutable,
                ).map(|r| r.rows).unwrap_or_default();
                rows.iter()
                    .map(|r| ImportEdge {
                        from_file: dv_str(&r[0]),
                        to_module: dv_str(&r[1]),
                        context: dv_str(&r[2]),
                    })
                    .collect()
            },
            call_edges: {
                let rows = self.run_script(
                    "?[caller, callee, file_path, line, context] := *calls_symbol{workspace: $ws, caller, callee, state: $st, file_path, line, context @ 'NOW'}",
                    params_map(&[("ws", &ws), ("st", state)]),
                    ScriptMutability::Immutable,
                ).map(|r| r.rows).unwrap_or_default();
                rows.iter()
                    .map(|r| CallEdge {
                        caller: dv_str(&r[0]),
                        callee: dv_str(&r[1]),
                        file_path: dv_str(&r[2]),
                        line: i64_to_usize_saturating(dv_i64(&r[3])),
                        context: dv_str(&r[4]),
                    })
                    .collect()
            },
            reference_edges: {
                let rows = self.run_script(
                    "?[from_file, to_path, reference_kind, line, context] := *reference_edge{workspace: $ws, from_file, to_path, reference_kind, line, state: $st, context @ 'NOW'}",
                    params_map(&[("ws", &ws), ("st", state)]),
                    ScriptMutability::Immutable,
                ).map(|r| r.rows).unwrap_or_default();
                rows.iter()
                    .map(|r| ReferenceEdge {
                        from_file: dv_str(&r[0]),
                        to_path: dv_str(&r[1]),
                        reference_kind: dv_str(&r[2]),
                        line: i64_to_usize_saturating(dv_i64(&r[3])),
                        context: dv_str(&r[4]),
                    })
                    .collect()
            },
        }))
    }

    // ── Graph-native Query & Mutation Helpers ─────────────────────────────

    pub fn query_entity(&self, ws: &str, ctx: &str, name: &str) -> Option<Entity> {
        let ws = canonicalize_path(ws);
        let rows = self.run_script(
            "?[description, aggregate_root, file_path, start_line, end_line] := *entity{workspace: $ws, context: $ctx, name: $name, state: 'actual', description, aggregate_root, file_path, start_line, end_line @ 'NOW'}",
            params_map(&[("ws", &ws), ("ctx", ctx), ("name", name)]),
            ScriptMutability::Immutable,
        ).ok()?.rows;
        let row = rows.first()?;
        Some(Entity {
            name: name.to_string(),
            description: dv_str(&row[0]),
            aggregate_root: matches!(&row[1], cozo::DataValue::Bool(true)),
            fields: self.query_fields(&ws, ctx, "entity", name, "actual"),
            methods: self.query_methods(&ws, ctx, "entity", name, "actual"),
            invariants: self.query_invariants(&ws, ctx, name, "actual"),
            file_path: dv_opt_string(&row[2]),
            start_line: dv_opt_usize(&row[3]),
            end_line: dv_opt_usize(&row[4]),
        })
    }

    pub fn query_service(&self, ws: &str, ctx: &str, name: &str) -> Option<Service> {
        let ws = canonicalize_path(ws);
        let rows = self.run_script(
            "?[description, kind, file_path, start_line, end_line] := *service{workspace: $ws, context: $ctx, name: $name, state: 'actual', description, kind, file_path, start_line, end_line @ 'NOW'}",
            params_map(&[("ws", &ws), ("ctx", ctx), ("name", name)]),
            ScriptMutability::Immutable,
        ).ok()?.rows;
        let row = rows.first()?;
        let dep_rows = self.run_script(
            "?[dep] := *service_dep{workspace: $ws, context: $ctx, service: $name, dep, state: 'actual' @ 'NOW'}",
            params_map(&[("ws", &ws), ("ctx", ctx), ("name", name)]),
            ScriptMutability::Immutable,
        ).map(|r| r.rows).unwrap_or_default();
        Some(Service {
            name: name.to_string(),
            description: dv_str(&row[0]),
            kind: match dv_str(&row[1]).as_str() {
                "application" => ServiceKind::Application,
                "infrastructure" => ServiceKind::Infrastructure,
                _ => ServiceKind::Domain,
            },
            methods: self.query_methods(&ws, ctx, "service", name, "actual"),
            dependencies: dep_rows.iter().map(|r| dv_str(&r[0])).collect(),
            file_path: dv_opt_string(&row[2]),
            start_line: dv_opt_usize(&row[3]),
            end_line: dv_opt_usize(&row[4]),
        })
    }

    pub fn query_event(&self, ws: &str, ctx: &str, name: &str) -> Option<DomainEvent> {
        let ws = canonicalize_path(ws);
        let rows = self.run_script(
            "?[description, source, file_path, start_line, end_line] := *event{workspace: $ws, context: $ctx, name: $name, state: 'actual', description, source, file_path, start_line, end_line @ 'NOW'}",
            params_map(&[("ws", &ws), ("ctx", ctx), ("name", name)]),
            ScriptMutability::Immutable,
        ).ok()?.rows;
        let row = rows.first()?;
        Some(DomainEvent {
            name: name.to_string(),
            description: dv_str(&row[0]),
            fields: self.query_fields(&ws, ctx, "event", name, "actual"),
            source: dv_str(&row[1]),
            file_path: dv_opt_string(&row[2]),
            start_line: dv_opt_usize(&row[3]),
            end_line: dv_opt_usize(&row[4]),
        })
    }

    pub fn query_value_object(&self, ws: &str, ctx: &str, name: &str) -> Option<ValueObject> {
        let ws = canonicalize_path(ws);
        let rows = self.run_script(
            "?[description, file_path, start_line, end_line] := *value_object{workspace: $ws, context: $ctx, name: $name, state: 'actual', description, file_path, start_line, end_line @ 'NOW'}",
            params_map(&[("ws", &ws), ("ctx", ctx), ("name", name)]),
            ScriptMutability::Immutable,
        ).ok()?.rows;
        let row = rows.first()?;
        Some(ValueObject {
            name: name.to_string(),
            description: dv_str(&row[0]),
            fields: self.query_fields(&ws, ctx, "value_object", name, "actual"),
            validation_rules: self.query_vo_rules(&ws, ctx, name, "actual"),
            file_path: dv_opt_string(&row[1]),
            start_line: dv_opt_usize(&row[2]),
            end_line: dv_opt_usize(&row[3]),
        })
    }

    pub fn query_repository(&self, ws: &str, ctx: &str, name: &str) -> Option<Repository> {
        let ws = canonicalize_path(ws);
        let rows = self.run_script(
            "?[aggregate, file_path, start_line, end_line] := *repository{workspace: $ws, context: $ctx, name: $name, state: 'actual', aggregate, file_path, start_line, end_line @ 'NOW'}",
            params_map(&[("ws", &ws), ("ctx", ctx), ("name", name)]),
            ScriptMutability::Immutable,
        ).ok()?.rows;
        let row = rows.first()?;
        Some(Repository {
            name: name.to_string(),
            aggregate: dv_str(&row[0]),
            methods: self.query_methods(&ws, ctx, "repository", name, "actual"),
            file_path: dv_opt_string(&row[1]),
            start_line: dv_opt_usize(&row[2]),
            end_line: dv_opt_usize(&row[3]),
        })
    }

    pub fn query_aggregate(&self, ws: &str, ctx: &str, name: &str) -> Option<Aggregate> {
        let ws = canonicalize_path(ws);
        let rows = self.run_script(
            "?[description, root_entity] := *aggregate{workspace: $ws, context: $ctx, name: $name, state: 'actual', description, root_entity @ 'NOW'}",
            params_map(&[("ws", &ws), ("ctx", ctx), ("name", name)]),
            ScriptMutability::Immutable,
        ).ok()?.rows;
        let row = rows.first()?;
        let members = self.run_script(
            "?[member_kind, member] := *aggregate_member{workspace: $ws, context: $ctx, aggregate: $name, member_kind, member, state: 'actual' @ 'NOW'}",
            params_map(&[("ws", &ws), ("ctx", ctx), ("name", name)]),
            ScriptMutability::Immutable,
        ).map(|r| r.rows).unwrap_or_default();
        Some(Aggregate {
            name: name.to_string(),
            description: dv_str(&row[0]),
            root_entity: dv_str(&row[1]),
            entities: members
                .iter()
                .filter(|r| dv_str(&r[0]) == "entity")
                .map(|r| dv_str(&r[1]))
                .collect(),
            value_objects: members
                .iter()
                .filter(|r| dv_str(&r[0]) == "value_object")
                .map(|r| dv_str(&r[1]))
                .collect(),
            ownership: self.query_ownership(&ws, ctx, "aggregate", name, "actual"),
        })
    }

    pub fn query_policy(&self, ws: &str, ctx: &str, name: &str) -> Option<Policy> {
        let ws = canonicalize_path(ws);
        let rows = self.run_script(
            "?[description, kind] := *policy{workspace: $ws, context: $ctx, name: $name, state: 'actual', description, kind @ 'NOW'}",
            params_map(&[("ws", &ws), ("ctx", ctx), ("name", name)]),
            ScriptMutability::Immutable,
        ).ok()?.rows;
        let row = rows.first()?;
        let links = self.run_script(
            "?[idx, link_kind, link] := *policy_link{workspace: $ws, context: $ctx, policy: $name, idx, state: 'actual', link_kind, link @ 'NOW'}",
            params_map(&[("ws", &ws), ("ctx", ctx), ("name", name)]),
            ScriptMutability::Immutable,
        ).map(|r| r.rows).unwrap_or_default();
        let mut indexed = links
            .iter()
            .map(|r| (dv_i64(&r[0]), dv_str(&r[1]), dv_str(&r[2])))
            .collect::<Vec<_>>();
        indexed.sort_by_key(|(idx, _, _)| *idx);
        Some(Policy {
            name: name.to_string(),
            description: dv_str(&row[0]),
            kind: match dv_str(&row[1]).as_str() {
                "process_manager" => PolicyKind::ProcessManager,
                "integration" => PolicyKind::Integration,
                _ => PolicyKind::Domain,
            },
            triggers: indexed
                .iter()
                .filter(|(_, kind, _)| kind == "trigger")
                .map(|(_, _, link)| link.clone())
                .collect(),
            commands: indexed
                .iter()
                .filter(|(_, kind, _)| kind == "command")
                .map(|(_, _, link)| link.clone())
                .collect(),
            ownership: self.query_ownership(&ws, ctx, "policy", name, "actual"),
        })
    }

    pub fn query_read_model(&self, ws: &str, ctx: &str, name: &str) -> Option<ReadModel> {
        let ws = canonicalize_path(ws);
        let rows = self.run_script(
            "?[description, source] := *read_model{workspace: $ws, context: $ctx, name: $name, state: 'actual', description, source @ 'NOW'}",
            params_map(&[("ws", &ws), ("ctx", ctx), ("name", name)]),
            ScriptMutability::Immutable,
        ).ok()?.rows;
        let row = rows.first()?;
        Some(ReadModel {
            name: name.to_string(),
            description: dv_str(&row[0]),
            source: dv_str(&row[1]),
            fields: self.query_fields(&ws, ctx, "read_model", name, "actual"),
            ownership: self.query_ownership(&ws, ctx, "read_model", name, "actual"),
        })
    }

    pub fn query_external_system(&self, ws: &str, name: &str) -> Option<ExternalSystem> {
        let ws = canonicalize_path(ws);
        let rows = self.run_script(
            "?[description, kind, rationale] := *external_system{workspace: $ws, name: $name, state: 'actual', description, kind, rationale @ 'NOW'}",
            params_map(&[("ws", &ws), ("name", name)]),
            ScriptMutability::Immutable,
        ).ok()?.rows;
        let row = rows.first()?;
        Some(ExternalSystem {
            name: name.to_string(),
            description: dv_str(&row[0]),
            kind: dv_str(&row[1]),
            consumed_by_contexts: self.query_indexed_strings(
                "?[idx, context] := *external_system_context{workspace: $ws, system: $name, idx, state: 'actual', context @ 'NOW'}",
                params_map(&[("ws", &ws), ("name", name)]),
            ),
            rationale: dv_str(&row[2]),
            ownership: self.query_ownership(&ws, "", "external_system", name, "actual"),
        })
    }

    pub fn query_architectural_decision(
        &self,
        ws: &str,
        id: &str,
    ) -> Option<ArchitecturalDecision> {
        let ws = canonicalize_path(ws);
        let rows = self.run_script(
            "?[title, status, scope, date, rationale] := *architectural_decision{workspace: $ws, id: $id, state: 'actual', title, status, scope, date, rationale @ 'NOW'}",
            params_map(&[("ws", &ws), ("id", id)]),
            ScriptMutability::Immutable,
        ).ok()?.rows;
        let row = rows.first()?;
        Some(ArchitecturalDecision {
            id: id.to_string(),
            title: dv_str(&row[0]),
            status: match dv_str(&row[1]).as_str() {
                "accepted" => DecisionStatus::Accepted,
                "superseded" => DecisionStatus::Superseded,
                "deprecated" => DecisionStatus::Deprecated,
                _ => DecisionStatus::Proposed,
            },
            scope: dv_str(&row[2]),
            date: dv_str(&row[3]),
            rationale: dv_str(&row[4]),
            consequences: self.query_indexed_strings(
                "?[idx, text] := *decision_consequence{workspace: $ws, decision_id: $id, idx, state: 'actual', text @ 'NOW'}",
                params_map(&[("ws", &ws), ("id", id)]),
            ),
            contexts: self.query_indexed_strings(
                "?[idx, context] := *decision_context{workspace: $ws, decision_id: $id, idx, state: 'actual', context @ 'NOW'}",
                params_map(&[("ws", &ws), ("id", id)]),
            ),
            ownership: self.query_ownership(&ws, "", "architectural_decision", id, "actual"),
        })
    }

    pub fn upsert_context(
        &self,
        workspace_path: &str,
        name: &str,
        description: &str,
        module_path: &str,
        dependencies: &[String],
        ownership: &Ownership,
    ) -> Result<()> {
        let ws = canonicalize_path(workspace_path);
        self.ensure_project(workspace_path)?;
        self.run_script(
            "?[workspace, name, state, description, module_path] <- [[$ws, $name, 'actual', $desc, $mp]] :put context { workspace, name, state => description, module_path }",
            params_map(&[("ws", &ws), ("name", name), ("desc", description), ("mp", module_path)]),
            ScriptMutability::Mutable,
        ).map_err(|e| anyhow::anyhow!("upsert_context: {:?}", e))?;
        self.run_mutation_script(
            "?[workspace, from_ctx, to_ctx, state, vld] := *context_dep{workspace, from_ctx, to_ctx, state @ 'NOW'}, workspace = $ws, from_ctx = $name, state = 'actual', vld = 'RETRACT' :put context_dep { workspace, from_ctx, to_ctx, state, vld }",
            params_map(&[("ws", &ws), ("name", name)]),
            format!("retract context dependencies for {name}"),
        )?;
        for dep in dependencies {
            self.run_script(
                "?[workspace, from_ctx, to_ctx, state] <- [[$ws, $from, $to, 'actual']] :put context_dep { workspace, from_ctx, to_ctx, state }",
                params_map(&[("ws", &ws), ("from", name), ("to", dep)]),
                ScriptMutability::Mutable,
            ).map_err(|e| anyhow::anyhow!("upsert_context dep: {:?}", e))?;
        }
        self.save_owner_meta(&ws, name, "context", name, ownership, "actual")?;
        Ok(())
    }

    pub fn remove_context(&self, workspace_path: &str, name: &str) -> Result<bool> {
        let ws = canonicalize_path(workspace_path);
        let p = params_map(&[("ws", &ws), ("name", name)]);
        let exists = self
            .run_script(
                "?[n] := *context{workspace: $ws, name: $name, state: 'actual' @ 'NOW'}, n = $name",
                p.clone(),
                ScriptMutability::Immutable,
            )
            .map(|r| !r.rows.is_empty())
            .unwrap_or(false);
        if !exists {
            return Ok(false);
        }
        self.run_mutation_script(
            "?[workspace, from_ctx, to_ctx, state, vld] := *context_dep{workspace, from_ctx, to_ctx, state @ 'NOW'}, workspace = $ws, from_ctx = $name, state = 'actual', vld = 'RETRACT' :put context_dep { workspace, from_ctx, to_ctx, state, vld }",
            p.clone(),
            format!("remove outgoing context dependencies for {name}"),
        )?;
        self.run_mutation_script(
            "?[workspace, from_ctx, to_ctx, state, vld] := *context_dep{workspace, from_ctx, to_ctx, state @ 'NOW'}, workspace = $ws, to_ctx = $name, state = 'actual', vld = 'RETRACT' :put context_dep { workspace, from_ctx, to_ctx, state, vld }",
            p.clone(),
            format!("remove incoming context dependencies for {name}"),
        )?;
        self.remove_owner_meta(&ws, name, "context", name)?;
        self.run_script(
            "?[workspace, name, state, vld] := workspace = $ws, name = $name, state = 'actual', vld = 'RETRACT' :put context { workspace, name, state, vld }",
            p,
            ScriptMutability::Mutable,
        ).map_err(|e| anyhow::anyhow!("remove_context: {:?}", e))?;
        Ok(true)
    }

    pub fn upsert_entity(&self, workspace_path: &str, ctx: &str, entity: &Entity) -> Result<()> {
        let ws = canonicalize_path(workspace_path);
        self.ensure_project(workspace_path)?;
        let mut params = params_map(&[
            ("ws", &ws),
            ("ctx", ctx),
            ("name", &entity.name),
            ("desc", &entity.description),
        ]);
        params.insert(
            "aggregate_root".into(),
            cozo::DataValue::Bool(entity.aggregate_root),
        );
        params.insert(
            "file".into(),
            cozo::DataValue::Str(entity.file_path.as_deref().unwrap_or("").into()),
        );
        params.insert(
            "sl".into(),
            int_dv(usize_to_i64(entity.start_line.unwrap_or(0))),
        );
        params.insert(
            "el".into(),
            int_dv(usize_to_i64(entity.end_line.unwrap_or(0))),
        );
        self.run_script(
            "?[workspace, context, name, state, description, aggregate_root, file_path, start_line, end_line] <- [[$ws, $ctx, $name, 'actual', $desc, $aggregate_root, $file, $sl, $el]] :put entity { workspace, context, name, state => description, aggregate_root, file_path, start_line, end_line }",
            params,
            ScriptMutability::Mutable,
        ).map_err(|e| anyhow::anyhow!("upsert_entity: {:?}", e))?;
        self.replace_owner_fields(&ws, ctx, "entity", &entity.name, &entity.fields)?;
        self.replace_owner_methods(&ws, ctx, "entity", &entity.name, &entity.methods)?;
        self.replace_invariants(&ws, ctx, &entity.name, &entity.invariants)?;
        Ok(())
    }

    pub fn remove_entity(&self, workspace_path: &str, ctx: &str, name: &str) -> Result<bool> {
        let ws = canonicalize_path(workspace_path);
        let p = params_map(&[("ws", &ws), ("ctx", ctx), ("name", name)]);
        let exists = self.run_script(
            "?[n] := *entity{workspace: $ws, context: $ctx, name: $name, state: 'actual' @ 'NOW'}, n = $name",
            p.clone(),
            ScriptMutability::Immutable,
        ).map(|r| !r.rows.is_empty()).unwrap_or(false);
        if !exists {
            return Ok(false);
        }
        self.run_mutation_script("?[workspace, context, owner_kind, owner, name, state, vld] := *field{workspace, context, owner_kind, owner, name, state @ 'NOW'}, workspace = $ws, context = $ctx, owner_kind = 'entity', owner = $name, state = 'actual', vld = 'RETRACT' :put field { workspace, context, owner_kind, owner, name, state, vld }", p.clone(), format!("remove entity fields for {name}"))?;
        self.run_mutation_script("?[workspace, context, owner_kind, owner, name, state, vld] := *method{workspace, context, owner_kind, owner, name, state @ 'NOW'}, workspace = $ws, context = $ctx, owner_kind = 'entity', owner = $name, state = 'actual', vld = 'RETRACT' :put method { workspace, context, owner_kind, owner, name, state, vld }", p.clone(), format!("remove entity methods for {name}"))?;
        self.run_mutation_script("?[workspace, context, owner_kind, owner, method, name, state, vld] := *method_param{workspace, context, owner_kind, owner, method, name, state @ 'NOW'}, workspace = $ws, context = $ctx, owner_kind = 'entity', owner = $name, state = 'actual', vld = 'RETRACT' :put method_param { workspace, context, owner_kind, owner, method, name, state, vld }", p.clone(), format!("remove entity method params for {name}"))?;
        self.run_mutation_script("?[workspace, context, entity, idx, state, text, vld] := *invariant{workspace, context, entity, idx, state, text @ 'NOW'}, workspace = $ws, context = $ctx, entity = $name, state = 'actual', vld = 'RETRACT' :put invariant { workspace, context, entity, idx, state, vld => text }", p.clone(), format!("remove entity invariants for {name}"))?;
        self.run_script("?[workspace, context, name, state, vld] := workspace = $ws, context = $ctx, name = $name, state = 'actual', vld = 'RETRACT' :put entity { workspace, context, name, state, vld }", p, ScriptMutability::Mutable).map_err(|e| anyhow::anyhow!("remove_entity: {:?}", e))?;
        Ok(true)
    }

    pub fn upsert_api_endpoint(
        &self,
        workspace_path: &str,
        ctx: &str,
        ep: &APIEndpoint,
    ) -> Result<()> {
        let ws = canonicalize_path(workspace_path);
        self.ensure_project(workspace_path)?;
        let params = params_map(&[
            ("ws", &ws),
            ("ctx", ctx),
            ("id", &ep.id),
            ("svc", &ep.service_id),
            ("met", &ep.method),
            ("path", &ep.route_pattern),
            ("desc", &ep.description),
        ]);
        self.run_script(
            "?[workspace, context, id, state, service_id, method, route_pattern, description] <- \
             [[$ws, $ctx, $id, 'actual', $svc, $met, $path, $desc]] :put api_endpoint { workspace, context, id, state => service_id, method, route_pattern, description }",
            params, ScriptMutability::Mutable
        ).map_err(|e| anyhow::anyhow!("upsert_api_endpoint: {:?}", e))?;
        Ok(())
    }

    pub fn remove_api_endpoint(&self, workspace_path: &str, ctx: &str, id: &str) -> Result<bool> {
        let ws = canonicalize_path(workspace_path);
        let params = params_map(&[("ws", &ws), ("ctx", ctx), ("id", id)]);
        let _ = self.run_script(
            "?[workspace, context, id, state, vld] := *api_endpoint{workspace, context, id, state @ 'NOW'}, workspace = $ws, context = $ctx, id = $id, state = 'actual', vld = 'RETRACT' :put api_endpoint { workspace, context, id, state, vld }",
            params, ScriptMutability::Mutable
        ).map_err(|e| anyhow::anyhow!("remove_api_endpoint: {:?}", e))?;
        Ok(true)
    }

    pub fn query_api_endpoint(&self, ws: &str, ctx: &str, id: &str) -> Option<APIEndpoint> {
        let ws = canonicalize_path(ws);
        let rows = self.run_script(
            "?[service_id, method, route_pattern, description] := *api_endpoint{workspace: $ws, context: $ctx, id: $id, state: 'actual', service_id, method, route_pattern, description @ 'NOW'}",
            params_map(&[("ws", &ws), ("ctx", ctx), ("id", id)]),
            ScriptMutability::Immutable
        ).ok()?.rows;
        let row = rows.first()?;
        Some(APIEndpoint {
            id: id.to_string(),
            service_id: dv_str(&row[0]),
            method: dv_str(&row[1]),
            route_pattern: dv_str(&row[2]),
            description: dv_str(&row[3]),
        })
    }

    pub fn upsert_service(&self, workspace_path: &str, ctx: &str, service: &Service) -> Result<()> {
        let ws = canonicalize_path(workspace_path);
        self.ensure_project(workspace_path)?;
        let kind = match service.kind {
            ServiceKind::Application => "application",
            ServiceKind::Infrastructure => "infrastructure",
            ServiceKind::Domain => "domain",
        };
        let mut params = params_map(&[
            ("ws", &ws),
            ("ctx", ctx),
            ("name", &service.name),
            ("desc", &service.description),
            ("kind", kind),
        ]);
        params.insert(
            "file".into(),
            cozo::DataValue::Str(service.file_path.as_deref().unwrap_or("").into()),
        );
        params.insert(
            "sl".into(),
            int_dv(usize_to_i64(service.start_line.unwrap_or(0))),
        );
        params.insert(
            "el".into(),
            int_dv(usize_to_i64(service.end_line.unwrap_or(0))),
        );
        self.run_script(
            "?[workspace, context, name, state, description, kind, file_path, start_line, end_line] <- [[$ws, $ctx, $name, 'actual', $desc, $kind, $file, $sl, $el]] :put service { workspace, context, name, state => description, kind, file_path, start_line, end_line }",
            params,
            ScriptMutability::Mutable,
        ).map_err(|e| anyhow::anyhow!("upsert_service: {:?}", e))?;
        self.replace_owner_methods(&ws, ctx, "service", &service.name, &service.methods)?;
        self.replace_service_deps(&ws, ctx, &service.name, &service.dependencies)?;
        Ok(())
    }

    pub fn remove_service(&self, workspace_path: &str, ctx: &str, name: &str) -> Result<bool> {
        let ws = canonicalize_path(workspace_path);
        let p = params_map(&[("ws", &ws), ("ctx", ctx), ("name", name)]);
        let exists = self.run_script("?[n] := *service{workspace: $ws, context: $ctx, name: $name, state: 'actual' @ 'NOW'}, n = $name", p.clone(), ScriptMutability::Immutable).map(|r| !r.rows.is_empty()).unwrap_or(false);
        if !exists {
            return Ok(false);
        }
        self.run_mutation_script("?[workspace, context, owner_kind, owner, name, state, vld] := *method{workspace, context, owner_kind, owner, name, state @ 'NOW'}, workspace = $ws, context = $ctx, owner_kind = 'service', owner = $name, state = 'actual', vld = 'RETRACT' :put method { workspace, context, owner_kind, owner, name, state, vld }", p.clone(), format!("remove service methods for {name}"))?;
        self.run_mutation_script("?[workspace, context, owner_kind, owner, method, name, state, vld] := *method_param{workspace, context, owner_kind, owner, method, name, state @ 'NOW'}, workspace = $ws, context = $ctx, owner_kind = 'service', owner = $name, state = 'actual', vld = 'RETRACT' :put method_param { workspace, context, owner_kind, owner, method, name, state, vld }", p.clone(), format!("remove service method params for {name}"))?;
        self.run_mutation_script("?[workspace, context, service, dep, state, vld] := *service_dep{workspace, context, service, dep, state @ 'NOW'}, workspace = $ws, context = $ctx, service = $name, state = 'actual', vld = 'RETRACT' :put service_dep { workspace, context, service, dep, state, vld }", p.clone(), format!("remove service dependencies for {name}"))?;
        self.run_script("?[workspace, context, name, state, vld] := workspace = $ws, context = $ctx, name = $name, state = 'actual', vld = 'RETRACT' :put service { workspace, context, name, state, vld }", p, ScriptMutability::Mutable).map_err(|e| anyhow::anyhow!("remove_service: {:?}", e))?;
        Ok(true)
    }

    pub fn upsert_event(&self, workspace_path: &str, ctx: &str, event: &DomainEvent) -> Result<()> {
        let ws = canonicalize_path(workspace_path);
        self.ensure_project(workspace_path)?;
        let mut params = params_map(&[
            ("ws", &ws),
            ("ctx", ctx),
            ("name", &event.name),
            ("desc", &event.description),
            ("source", &event.source),
        ]);
        params.insert(
            "file".into(),
            cozo::DataValue::Str(event.file_path.as_deref().unwrap_or("").into()),
        );
        params.insert(
            "sl".into(),
            int_dv(usize_to_i64(event.start_line.unwrap_or(0))),
        );
        params.insert(
            "el".into(),
            int_dv(usize_to_i64(event.end_line.unwrap_or(0))),
        );
        self.run_script(
            "?[workspace, context, name, state, description, source, file_path, start_line, end_line] <- [[$ws, $ctx, $name, 'actual', $desc, $source, $file, $sl, $el]] :put event { workspace, context, name, state => description, source, file_path, start_line, end_line }",
            params,
            ScriptMutability::Mutable,
        ).map_err(|e| anyhow::anyhow!("upsert_event: {:?}", e))?;
        self.replace_owner_fields(&ws, ctx, "event", &event.name, &event.fields)?;
        Ok(())
    }

    pub fn remove_event(&self, workspace_path: &str, ctx: &str, name: &str) -> Result<bool> {
        let ws = canonicalize_path(workspace_path);
        let p = params_map(&[("ws", &ws), ("ctx", ctx), ("name", name)]);
        let exists = self.run_script("?[n] := *event{workspace: $ws, context: $ctx, name: $name, state: 'actual' @ 'NOW'}, n = $name", p.clone(), ScriptMutability::Immutable).map(|r| !r.rows.is_empty()).unwrap_or(false);
        if !exists {
            return Ok(false);
        }
        self.run_mutation_script("?[workspace, context, owner_kind, owner, name, state, vld] := *field{workspace, context, owner_kind, owner, name, state @ 'NOW'}, workspace = $ws, context = $ctx, owner_kind = 'event', owner = $name, state = 'actual', vld = 'RETRACT' :put field { workspace, context, owner_kind, owner, name, state, vld }", p.clone(), format!("remove event fields for {name}"))?;
        self.run_script("?[workspace, context, name, state, vld] := workspace = $ws, context = $ctx, name = $name, state = 'actual', vld = 'RETRACT' :put event { workspace, context, name, state, vld }", p, ScriptMutability::Mutable).map_err(|e| anyhow::anyhow!("remove_event: {:?}", e))?;
        Ok(true)
    }

    pub fn upsert_value_object(
        &self,
        workspace_path: &str,
        ctx: &str,
        value_object: &ValueObject,
    ) -> Result<()> {
        let ws = canonicalize_path(workspace_path);
        self.ensure_project(workspace_path)?;
        let mut params = params_map(&[
            ("ws", &ws),
            ("ctx", ctx),
            ("name", &value_object.name),
            ("desc", &value_object.description),
        ]);
        params.insert(
            "file".into(),
            cozo::DataValue::Str(value_object.file_path.as_deref().unwrap_or("").into()),
        );
        params.insert(
            "sl".into(),
            int_dv(usize_to_i64(value_object.start_line.unwrap_or(0))),
        );
        params.insert(
            "el".into(),
            int_dv(usize_to_i64(value_object.end_line.unwrap_or(0))),
        );
        self.run_script(
            "?[workspace, context, name, state, description, file_path, start_line, end_line] <- [[$ws, $ctx, $name, 'actual', $desc, $file, $sl, $el]] :put value_object { workspace, context, name, state => description, file_path, start_line, end_line }",
            params,
            ScriptMutability::Mutable,
        ).map_err(|e| anyhow::anyhow!("upsert_value_object: {:?}", e))?;
        self.replace_owner_fields(
            &ws,
            ctx,
            "value_object",
            &value_object.name,
            &value_object.fields,
        )?;
        self.replace_vo_rules(&ws, ctx, &value_object.name, &value_object.validation_rules)?;
        Ok(())
    }

    pub fn remove_value_object(&self, workspace_path: &str, ctx: &str, name: &str) -> Result<bool> {
        let ws = canonicalize_path(workspace_path);
        let p = params_map(&[("ws", &ws), ("ctx", ctx), ("name", name)]);
        let exists = self.run_script("?[n] := *value_object{workspace: $ws, context: $ctx, name: $name, state: 'actual' @ 'NOW'}, n = $name", p.clone(), ScriptMutability::Immutable).map(|r| !r.rows.is_empty()).unwrap_or(false);
        if !exists {
            return Ok(false);
        }
        self.run_mutation_script("?[workspace, context, owner_kind, owner, name, state, vld] := *field{workspace, context, owner_kind, owner, name, state @ 'NOW'}, workspace = $ws, context = $ctx, owner_kind = 'value_object', owner = $name, state = 'actual', vld = 'RETRACT' :put field { workspace, context, owner_kind, owner, name, state, vld }", p.clone(), format!("remove value object fields for {name}"))?;
        self.run_mutation_script("?[workspace, context, value_object, idx, state, text, vld] := *vo_rule{workspace, context, value_object, idx, state, text @ 'NOW'}, workspace = $ws, context = $ctx, value_object = $name, state = 'actual', vld = 'RETRACT' :put vo_rule { workspace, context, value_object, idx, state, vld => text }", p.clone(), format!("remove value object rules for {name}"))?;
        self.run_script("?[workspace, context, name, state, vld] := workspace = $ws, context = $ctx, name = $name, state = 'actual', vld = 'RETRACT' :put value_object { workspace, context, name, state, vld }", p, ScriptMutability::Mutable).map_err(|e| anyhow::anyhow!("remove_value_object: {:?}", e))?;
        Ok(true)
    }

    pub fn upsert_repository(
        &self,
        workspace_path: &str,
        ctx: &str,
        repository: &Repository,
    ) -> Result<()> {
        let ws = canonicalize_path(workspace_path);
        self.ensure_project(workspace_path)?;
        let mut params = params_map(&[
            ("ws", &ws),
            ("ctx", ctx),
            ("name", &repository.name),
            ("aggregate", &repository.aggregate),
        ]);
        params.insert(
            "file".into(),
            cozo::DataValue::Str(repository.file_path.as_deref().unwrap_or("").into()),
        );
        params.insert(
            "sl".into(),
            int_dv(usize_to_i64(repository.start_line.unwrap_or(0))),
        );
        params.insert(
            "el".into(),
            int_dv(usize_to_i64(repository.end_line.unwrap_or(0))),
        );
        self.run_script(
            "?[workspace, context, name, state, aggregate, file_path, start_line, end_line] <- [[$ws, $ctx, $name, 'actual', $aggregate, $file, $sl, $el]] :put repository { workspace, context, name, state => aggregate, file_path, start_line, end_line }",
            params,
            ScriptMutability::Mutable,
        ).map_err(|e| anyhow::anyhow!("upsert_repository: {:?}", e))?;
        self.replace_owner_methods(
            &ws,
            ctx,
            "repository",
            &repository.name,
            &repository.methods,
        )?;
        Ok(())
    }

    pub fn remove_repository(&self, workspace_path: &str, ctx: &str, name: &str) -> Result<bool> {
        let ws = canonicalize_path(workspace_path);
        let p = params_map(&[("ws", &ws), ("ctx", ctx), ("name", name)]);
        let exists = self.run_script("?[n] := *repository{workspace: $ws, context: $ctx, name: $name, state: 'actual' @ 'NOW'}, n = $name", p.clone(), ScriptMutability::Immutable).map(|r| !r.rows.is_empty()).unwrap_or(false);
        if !exists {
            return Ok(false);
        }
        self.run_mutation_script("?[workspace, context, owner_kind, owner, name, state, vld] := *method{workspace, context, owner_kind, owner, name, state @ 'NOW'}, workspace = $ws, context = $ctx, owner_kind = 'repository', owner = $name, state = 'actual', vld = 'RETRACT' :put method { workspace, context, owner_kind, owner, name, state, vld }", p.clone(), format!("remove repository methods for {name}"))?;
        self.run_mutation_script("?[workspace, context, owner_kind, owner, method, name, state, vld] := *method_param{workspace, context, owner_kind, owner, method, name, state @ 'NOW'}, workspace = $ws, context = $ctx, owner_kind = 'repository', owner = $name, state = 'actual', vld = 'RETRACT' :put method_param { workspace, context, owner_kind, owner, method, name, state, vld }", p.clone(), format!("remove repository method params for {name}"))?;
        self.run_script("?[workspace, context, name, state, vld] := workspace = $ws, context = $ctx, name = $name, state = 'actual', vld = 'RETRACT' :put repository { workspace, context, name, state, vld }", p, ScriptMutability::Mutable).map_err(|e| anyhow::anyhow!("remove_repository: {:?}", e))?;
        Ok(true)
    }

    pub fn query_module(&self, ws: &str, ctx: &str, name: &str) -> Option<Module> {
        let ws = canonicalize_path(ws);
        let rows = self.run_script(
            "?[path, public, file_path, description] := *module{workspace: $ws, context: $ctx, name: $name, state: 'actual', path, public, file_path, description @ 'NOW'}",
            params_map(&[("ws", &ws), ("ctx", ctx), ("name", name)]),
            ScriptMutability::Immutable,
        ).ok()?.rows;
        let row = rows.first()?;
        Some(Module {
            name: name.to_string(),
            path: dv_str(&row[0]),
            public: matches!(&row[1], cozo::DataValue::Bool(true)),
            file_path: dv_str(&row[2]),
            description: dv_str(&row[3]),
        })
    }

    pub fn upsert_module(&self, workspace_path: &str, ctx: &str, module: &Module) -> Result<()> {
        let ws = canonicalize_path(workspace_path);
        self.ensure_project(workspace_path)?;
        let mut params = params_map(&[
            ("ws", &ws),
            ("ctx", ctx),
            ("name", &module.name),
            ("path", &module.path),
            ("fp", &module.file_path),
            ("desc", &module.description),
        ]);
        params.insert("public".into(), cozo::DataValue::Bool(module.public));
        self.run_script(
            "?[workspace, context, name, state, path, public, file_path, description] <- [[$ws, $ctx, $name, 'actual', $path, $public, $fp, $desc]] :put module { workspace, context, name, state => path, public, file_path, description }",
            params,
            ScriptMutability::Mutable,
        ).map_err(|e| anyhow::anyhow!("upsert_module: {:?}", e))?;
        Ok(())
    }

    pub fn remove_module(&self, workspace_path: &str, ctx: &str, name: &str) -> Result<bool> {
        let ws = canonicalize_path(workspace_path);
        let p = params_map(&[("ws", &ws), ("ctx", ctx), ("name", name)]);
        let exists = self.run_script(
            "?[n] := *module{workspace: $ws, context: $ctx, name: $name, state: 'actual' @ 'NOW'}, n = $name",
            p.clone(),
            ScriptMutability::Immutable,
        ).map(|r| !r.rows.is_empty()).unwrap_or(false);
        if !exists {
            return Ok(false);
        }
        self.run_script(
            "?[workspace, context, name, state, vld] := workspace = $ws, context = $ctx, name = $name, state = 'actual', vld = 'RETRACT' :put module { workspace, context, name, state, vld }",
            p,
            ScriptMutability::Mutable,
        ).map_err(|e| anyhow::anyhow!("remove_module: {:?}", e))?;
        Ok(true)
    }

    pub fn upsert_aggregate(
        &self,
        workspace_path: &str,
        ctx: &str,
        aggregate: &Aggregate,
    ) -> Result<()> {
        let ws = canonicalize_path(workspace_path);
        self.run_script(
            "?[workspace, context, name, state, description, root_entity] <- [[$ws, $ctx, $name, 'actual', $desc, $root]] :put aggregate { workspace, context, name, state => description, root_entity }",
            params_map(&[("ws", &ws), ("ctx", ctx), ("name", &aggregate.name), ("desc", &aggregate.description), ("root", &aggregate.root_entity)]),
            ScriptMutability::Mutable,
        ).map_err(|e| anyhow::anyhow!("upsert_aggregate: {:?}", e))?;
        self.save_owner_meta(
            &ws,
            ctx,
            "aggregate",
            &aggregate.name,
            &aggregate.ownership,
            "actual",
        )?;
        self.run_mutation_script(
            "?[workspace, context, aggregate, member_kind, member, state, vld] := *aggregate_member{workspace, context, aggregate, member_kind, member, state @ 'NOW'}, workspace = $ws, context = $ctx, aggregate = $name, state = 'actual', vld = 'RETRACT' :put aggregate_member { workspace, context, aggregate, member_kind, member, state, vld }",
            params_map(&[("ws", &ws), ("ctx", ctx), ("name", &aggregate.name)]),
            format!("retract aggregate members for {}", aggregate.name),
        )?;
        for entity in &aggregate.entities {
            self.run_script("?[workspace, context, aggregate, member_kind, member, state] <- [[$ws, $ctx, $name, 'entity', $member, 'actual']] :put aggregate_member { workspace, context, aggregate, member_kind, member, state }", params_map(&[("ws", &ws), ("ctx", ctx), ("name", &aggregate.name), ("member", entity)]), ScriptMutability::Mutable).map_err(|e| anyhow::anyhow!("upsert_aggregate entity: {:?}", e))?;
        }
        for vo in &aggregate.value_objects {
            self.run_script("?[workspace, context, aggregate, member_kind, member, state] <- [[$ws, $ctx, $name, 'value_object', $member, 'actual']] :put aggregate_member { workspace, context, aggregate, member_kind, member, state }", params_map(&[("ws", &ws), ("ctx", ctx), ("name", &aggregate.name), ("member", vo)]), ScriptMutability::Mutable).map_err(|e| anyhow::anyhow!("upsert_aggregate vo: {:?}", e))?;
        }
        Ok(())
    }

    pub fn remove_aggregate(&self, workspace_path: &str, ctx: &str, name: &str) -> Result<bool> {
        let ws = canonicalize_path(workspace_path);
        let p = params_map(&[("ws", &ws), ("ctx", ctx), ("name", name)]);
        let exists = self.run_script("?[n] := *aggregate{workspace: $ws, context: $ctx, name: $name, state: 'actual' @ 'NOW'}, n = $name", p.clone(), ScriptMutability::Immutable).map(|r| !r.rows.is_empty()).unwrap_or(false);
        if !exists {
            return Ok(false);
        }
        self.run_mutation_script("?[workspace, context, aggregate, member_kind, member, state, vld] := *aggregate_member{workspace, context, aggregate, member_kind, member, state @ 'NOW'}, workspace = $ws, context = $ctx, aggregate = $name, state = 'actual', vld = 'RETRACT' :put aggregate_member { workspace, context, aggregate, member_kind, member, state, vld }", p.clone(), format!("remove aggregate members for {name}"))?;
        self.remove_owner_meta(&ws, ctx, "aggregate", name)?;
        self.run_script("?[workspace, context, name, state, vld] := workspace = $ws, context = $ctx, name = $name, state = 'actual', vld = 'RETRACT' :put aggregate { workspace, context, name, state, vld }", p, ScriptMutability::Mutable).map_err(|e| anyhow::anyhow!("remove_aggregate: {:?}", e))?;
        Ok(true)
    }

    pub fn upsert_policy(&self, workspace_path: &str, ctx: &str, policy: &Policy) -> Result<()> {
        let ws = canonicalize_path(workspace_path);
        let kind = Self::policy_kind_key(&policy.kind).to_string();
        self.run_script("?[workspace, context, name, state, description, kind] <- [[$ws, $ctx, $name, 'actual', $desc, $kind]] :put policy { workspace, context, name, state => description, kind }", params_map(&[("ws", &ws), ("ctx", ctx), ("name", &policy.name), ("desc", &policy.description), ("kind", &kind)]), ScriptMutability::Mutable).map_err(|e| anyhow::anyhow!("upsert_policy: {:?}", e))?;
        self.save_owner_meta(
            &ws,
            ctx,
            "policy",
            &policy.name,
            &policy.ownership,
            "actual",
        )?;
        self.run_mutation_script("?[workspace, context, policy, link_kind, link, idx, state, vld] := *policy_link{workspace, context, policy, link_kind, link, idx, state @ 'NOW'}, workspace = $ws, context = $ctx, policy = $name, state = 'actual', vld = 'RETRACT' :put policy_link { workspace, context, policy, link_kind, link, idx, state, vld }", params_map(&[("ws", &ws), ("ctx", ctx), ("name", &policy.name)]), format!("retract policy links for {}", policy.name))?;
        for (idx, trigger) in policy.triggers.iter().enumerate() {
            let mut p = params_map(&[
                ("ws", &ws),
                ("ctx", ctx),
                ("name", &policy.name),
                ("link", trigger),
            ]);
            p.insert("idx".into(), int_dv(usize_to_i64(idx)));
            self.run_script("?[workspace, context, policy, link_kind, link, idx, state] <- [[$ws, $ctx, $name, 'trigger', $link, $idx, 'actual']] :put policy_link { workspace, context, policy, link_kind, link, idx, state }", p, ScriptMutability::Mutable).map_err(|e| anyhow::anyhow!("upsert_policy trigger: {:?}", e))?;
        }
        for (idx, command) in policy.commands.iter().enumerate() {
            let mut p = params_map(&[
                ("ws", &ws),
                ("ctx", ctx),
                ("name", &policy.name),
                ("link", command),
            ]);
            p.insert("idx".into(), int_dv(usize_to_i64(idx)));
            self.run_script("?[workspace, context, policy, link_kind, link, idx, state] <- [[$ws, $ctx, $name, 'command', $link, $idx, 'actual']] :put policy_link { workspace, context, policy, link_kind, link, idx, state }", p, ScriptMutability::Mutable).map_err(|e| anyhow::anyhow!("upsert_policy command: {:?}", e))?;
        }
        Ok(())
    }

    pub fn remove_policy(&self, workspace_path: &str, ctx: &str, name: &str) -> Result<bool> {
        let ws = canonicalize_path(workspace_path);
        let p = params_map(&[("ws", &ws), ("ctx", ctx), ("name", name)]);
        let exists = self.run_script("?[n] := *policy{workspace: $ws, context: $ctx, name: $name, state: 'actual' @ 'NOW'}, n = $name", p.clone(), ScriptMutability::Immutable).map(|r| !r.rows.is_empty()).unwrap_or(false);
        if !exists {
            return Ok(false);
        }
        self.run_mutation_script("?[workspace, context, policy, link_kind, link, idx, state, vld] := *policy_link{workspace, context, policy, link_kind, link, idx, state @ 'NOW'}, workspace = $ws, context = $ctx, policy = $name, state = 'actual', vld = 'RETRACT' :put policy_link { workspace, context, policy, link_kind, link, idx, state, vld }", p.clone(), format!("remove policy links for {name}"))?;
        self.remove_owner_meta(&ws, ctx, "policy", name)?;
        self.run_script("?[workspace, context, name, state, vld] := workspace = $ws, context = $ctx, name = $name, state = 'actual', vld = 'RETRACT' :put policy { workspace, context, name, state, vld }", p, ScriptMutability::Mutable).map_err(|e| anyhow::anyhow!("remove_policy: {:?}", e))?;
        Ok(true)
    }

    pub fn upsert_read_model(
        &self,
        workspace_path: &str,
        ctx: &str,
        read_model: &ReadModel,
    ) -> Result<()> {
        let ws = canonicalize_path(workspace_path);
        self.run_script("?[workspace, context, name, state, description, source] <- [[$ws, $ctx, $name, 'actual', $desc, $src]] :put read_model { workspace, context, name, state => description, source }", params_map(&[("ws", &ws), ("ctx", ctx), ("name", &read_model.name), ("desc", &read_model.description), ("src", &read_model.source)]), ScriptMutability::Mutable).map_err(|e| anyhow::anyhow!("upsert_read_model: {:?}", e))?;
        self.save_owner_meta(
            &ws,
            ctx,
            "read_model",
            &read_model.name,
            &read_model.ownership,
            "actual",
        )?;
        self.replace_owner_fields(&ws, ctx, "read_model", &read_model.name, &read_model.fields)?;
        Ok(())
    }

    pub fn remove_read_model(&self, workspace_path: &str, ctx: &str, name: &str) -> Result<bool> {
        let ws = canonicalize_path(workspace_path);
        let p = params_map(&[("ws", &ws), ("ctx", ctx), ("name", name)]);
        let exists = self.run_script("?[n] := *read_model{workspace: $ws, context: $ctx, name: $name, state: 'actual' @ 'NOW'}, n = $name", p.clone(), ScriptMutability::Immutable).map(|r| !r.rows.is_empty()).unwrap_or(false);
        if !exists {
            return Ok(false);
        }
        self.remove_owner_meta(&ws, ctx, "read_model", name)?;
        self.run_mutation_script("?[workspace, context, owner_kind, owner, name, state, vld] := *field{workspace, context, owner_kind, owner, name, state @ 'NOW'}, workspace = $ws, context = $ctx, owner_kind = 'read_model', owner = $name, state = 'actual', vld = 'RETRACT' :put field { workspace, context, owner_kind, owner, name, state, vld }", p.clone(), format!("remove read model fields for {name}"))?;
        self.run_script("?[workspace, context, name, state, vld] := workspace = $ws, context = $ctx, name = $name, state = 'actual', vld = 'RETRACT' :put read_model { workspace, context, name, state, vld }", p, ScriptMutability::Mutable).map_err(|e| anyhow::anyhow!("remove_read_model: {:?}", e))?;
        Ok(true)
    }

    pub fn upsert_external_system(
        &self,
        workspace_path: &str,
        system: &ExternalSystem,
    ) -> Result<()> {
        let ws = canonicalize_path(workspace_path);
        self.run_script("?[workspace, name, state, description, kind, rationale] <- [[$ws, $name, 'actual', $desc, $kind, $rationale]] :put external_system { workspace, name, state => description, kind, rationale }", params_map(&[("ws", &ws), ("name", &system.name), ("desc", &system.description), ("kind", &system.kind), ("rationale", &system.rationale)]), ScriptMutability::Mutable).map_err(|e| anyhow::anyhow!("upsert_external_system: {:?}", e))?;
        self.save_owner_meta(
            &ws,
            "",
            "external_system",
            &system.name,
            &system.ownership,
            "actual",
        )?;
        self.run_mutation_script("?[workspace, system, context, idx, state, vld] := *external_system_context{workspace, system, context, idx, state @ 'NOW'}, workspace = $ws, system = $name, state = 'actual', vld = 'RETRACT' :put external_system_context { workspace, system, context, idx, state, vld }", params_map(&[("ws", &ws), ("name", &system.name)]), format!("retract external system contexts for {}", system.name))?;
        for (idx, ctx) in system.consumed_by_contexts.iter().enumerate() {
            let mut p = params_map(&[("ws", &ws), ("name", &system.name), ("ctx", ctx)]);
            p.insert("idx".into(), int_dv(usize_to_i64(idx)));
            self.run_script("?[workspace, system, context, idx, state] <- [[$ws, $name, $ctx, $idx, 'actual']] :put external_system_context { workspace, system, context, idx, state }", p, ScriptMutability::Mutable).map_err(|e| anyhow::anyhow!("upsert_external_system ctx: {:?}", e))?;
        }
        Ok(())
    }

    pub fn remove_external_system(&self, workspace_path: &str, name: &str) -> Result<bool> {
        let ws = canonicalize_path(workspace_path);
        let p = params_map(&[("ws", &ws), ("name", name)]);
        let exists = self.run_script("?[n] := *external_system{workspace: $ws, name: $name, state: 'actual' @ 'NOW'}, n = $name", p.clone(), ScriptMutability::Immutable).map(|r| !r.rows.is_empty()).unwrap_or(false);
        if !exists {
            return Ok(false);
        }
        self.remove_owner_meta(&ws, "", "external_system", name)?;
        self.run_mutation_script("?[workspace, system, context, idx, state, vld] := *external_system_context{workspace, system, context, idx, state @ 'NOW'}, workspace = $ws, system = $name, state = 'actual', vld = 'RETRACT' :put external_system_context { workspace, system, context, idx, state, vld }", p.clone(), format!("remove external system contexts for {name}"))?;
        self.run_script("?[workspace, name, state, vld] := workspace = $ws, name = $name, state = 'actual', vld = 'RETRACT' :put external_system { workspace, name, state, vld }", p, ScriptMutability::Mutable).map_err(|e| anyhow::anyhow!("remove_external_system: {:?}", e))?;
        Ok(true)
    }

    pub fn upsert_architectural_decision(
        &self,
        workspace_path: &str,
        decision: &ArchitecturalDecision,
    ) -> Result<()> {
        let ws = canonicalize_path(workspace_path);
        let status = format!("{:?}", decision.status).to_lowercase();
        self.run_script("?[workspace, id, state, title, status, scope, date, rationale] <- [[$ws, $id, 'actual', $title, $status, $scope, $date, $rationale]] :put architectural_decision { workspace, id, state => title, status, scope, date, rationale }", params_map(&[("ws", &ws), ("id", &decision.id), ("title", &decision.title), ("status", &status), ("scope", &decision.scope), ("date", &decision.date), ("rationale", &decision.rationale)]), ScriptMutability::Mutable).map_err(|e| anyhow::anyhow!("upsert_architectural_decision: {:?}", e))?;
        self.save_owner_meta(
            &ws,
            "",
            "architectural_decision",
            &decision.id,
            &decision.ownership,
            "actual",
        )?;
        self.run_mutation_script("?[workspace, decision_id, context, idx, state, vld] := *decision_context{workspace, decision_id, context, idx, state @ 'NOW'}, workspace = $ws, decision_id = $id, state = 'actual', vld = 'RETRACT' :put decision_context { workspace, decision_id, context, idx, state, vld }", params_map(&[("ws", &ws), ("id", &decision.id)]), format!("retract decision contexts for {}", decision.id))?;
        self.run_mutation_script("?[workspace, decision_id, idx, state, vld] := *decision_consequence{workspace, decision_id, idx, state @ 'NOW'}, workspace = $ws, decision_id = $id, state = 'actual', vld = 'RETRACT' :put decision_consequence { workspace, decision_id, idx, state, vld }", params_map(&[("ws", &ws), ("id", &decision.id)]), format!("retract decision consequences for {}", decision.id))?;
        for (idx, ctx) in decision.contexts.iter().enumerate() {
            let mut p = params_map(&[("ws", &ws), ("id", &decision.id), ("ctx", ctx)]);
            p.insert("idx".into(), int_dv(usize_to_i64(idx)));
            self.run_script("?[workspace, decision_id, context, idx, state] <- [[$ws, $id, $ctx, $idx, 'actual']] :put decision_context { workspace, decision_id, context, idx, state }", p, ScriptMutability::Mutable).map_err(|e| anyhow::anyhow!("upsert_architectural_decision ctx: {:?}", e))?;
        }
        for (idx, consequence) in decision.consequences.iter().enumerate() {
            let mut p = params_map(&[("ws", &ws), ("id", &decision.id), ("text", consequence)]);
            p.insert("idx".into(), int_dv(usize_to_i64(idx)));
            self.run_script("?[workspace, decision_id, idx, state, text] <- [[$ws, $id, $idx, 'actual', $text]] :put decision_consequence { workspace, decision_id, idx, state => text }", p, ScriptMutability::Mutable).map_err(|e| anyhow::anyhow!("upsert_architectural_decision consequence: {:?}", e))?;
        }
        Ok(())
    }

    pub fn remove_architectural_decision(&self, workspace_path: &str, id: &str) -> Result<bool> {
        let ws = canonicalize_path(workspace_path);
        let p = params_map(&[("ws", &ws), ("id", id)]);
        let exists = self.run_script("?[n] := *architectural_decision{workspace: $ws, id: $id, state: 'actual' @ 'NOW'}, n = $id", p.clone(), ScriptMutability::Immutable).map(|r| !r.rows.is_empty()).unwrap_or(false);
        if !exists {
            return Ok(false);
        }
        self.remove_owner_meta(&ws, "", "architectural_decision", id)?;
        self.run_mutation_script("?[workspace, decision_id, context, idx, state, vld] := *decision_context{workspace, decision_id, context, idx, state @ 'NOW'}, workspace = $ws, decision_id = $id, state = 'actual', vld = 'RETRACT' :put decision_context { workspace, decision_id, context, idx, state, vld }", p.clone(), format!("remove decision contexts for {id}"))?;
        self.run_mutation_script("?[workspace, decision_id, idx, state, vld] := *decision_consequence{workspace, decision_id, idx, state @ 'NOW'}, workspace = $ws, decision_id = $id, state = 'actual', vld = 'RETRACT' :put decision_consequence { workspace, decision_id, idx, state, vld }", p.clone(), format!("remove decision consequences for {id}"))?;
        self.run_script("?[workspace, id, state, vld] := workspace = $ws, id = $id, state = 'actual', vld = 'RETRACT' :put architectural_decision { workspace, id, state, vld }", p, ScriptMutability::Mutable).map_err(|e| anyhow::anyhow!("remove_architectural_decision: {:?}", e))?;
        Ok(true)
    }

    // ── Project Operations ─────────────────────────────────────────────────

    /// List all stored projects.
    pub fn list(&self) -> Result<Vec<ProjectInfo>> {
        let result = self
            .run_script(
                "?[workspace, name, updated_at] := *project{workspace, name, updated_at}",
                Default::default(),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("Failed to list projects: {:?}", e))?;

        let mut projects: Vec<ProjectInfo> = result
            .rows
            .iter()
            .map(|r| ProjectInfo {
                workspace_path: dv_str(&r[0]),
                project_name: dv_str(&r[1]),
                updated_at: dv_str(&r[2]),
            })
            .collect();
        projects.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(projects)
    }

    /// Export a domain model to a JSON file.
    /// `state` can be `"actual"`, `"both"`, or a compatibility alias such as `"desired"`.
    pub fn export_to_file(&self, workspace_path: &str, file_path: &str, state: &str) -> Result<()> {
        let json = match canonical_model_state(state) {
            "actual" => {
                let model = self.load_actual(workspace_path)?.with_context(|| {
                    format!("No actual model found for workspace: {workspace_path}")
                })?;
                serde_json::to_string_pretty(&model)?
            }
            "both" => {
                let implemented = self.load_actual(workspace_path)?;
                let actual = self.load_actual(workspace_path)?;
                serde_json::to_string_pretty(&serde_json::json!({
                    "implemented": implemented,
                    "actual": actual
                }))?
            }
            _ => {
                let model = self.load_desired(workspace_path)?.with_context(|| {
                    format!("No implemented model found for workspace: {workspace_path}")
                })?;
                serde_json::to_string_pretty(&model)?
            }
        };
        std::fs::write(file_path, json)
            .with_context(|| format!("Failed to write file: {file_path}"))?;
        Ok(())
    }

    // ── Temporal Differencing ──────────────────────────────────────────────

    /// Compute the diff between the two most recent actual graph snapshots.
    pub fn diff_graph(&self, workspace_path: &str) -> Result<serde_json::Value> {
        let snapshots = self.list_snapshots(workspace_path, "actual")?;
        if snapshots.len() < 2 {
            return Ok(json!({
                "basis": "actual_history",
                "pending_changes": [],
                "summary": {
                    "total_changes": 0,
                    "additions": 0,
                    "removals": 0
                }
            }));
        }

        let ts_new = snapshots[0];
        let ts_old = snapshots[1];
        let diff = self.diff_snapshots(workspace_path, "actual", ts_old, ts_new)?;
        let mut pending_changes = Vec::new();
        if let Some(added) = diff.get("added").and_then(Value::as_array) {
            pending_changes.extend(added.iter().cloned());
        }
        if let Some(removed) = diff.get("removed").and_then(Value::as_array) {
            pending_changes.extend(removed.iter().cloned());
        }
        let total_changes = pending_changes.len();

        Ok(json!({
            "basis": "actual_history",
            "ts_old": ts_old,
            "ts_new": ts_new,
            "pending_changes": pending_changes,
            "summary": diff.get("summary").cloned().unwrap_or_else(|| json!({
                "total_changes": total_changes,
                "additions": 0,
                "removals": 0
            })),
            "added": diff.get("added").cloned().unwrap_or_else(|| json!([])),
            "removed": diff.get("removed").cloned().unwrap_or_else(|| json!([])),
        }))
    }

    /// Persist the latest actual-history diff to the drift relation.
    pub fn compute_drift(&self, workspace_path: &str) -> Result<usize> {
        self.with_write_lock(|| {
            let ws = canonicalize_path(workspace_path);
            let params = params_map(&[("ws", &ws)]);

            // 1. Retract previous drift entries
            self.run_mutation_script(
                "?[workspace, category, context, name, change_type, vld] := \
                 *drift{workspace, category, context, name, change_type @ 'NOW'}, workspace = $ws, vld = 'RETRACT' \
                 :put drift { workspace, category, context, name, change_type, vld }",
                params.clone(),
                format!("compute_drift retract previous drift entries for '{ws}'"),
            )?;

            // 2. Persist the most recent temporal diff as drift entries.
            let diff = self.diff_graph(workspace_path)?;
            let changes = diff
                .get("pending_changes")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();

            for change in &changes {
                let kind = change.get("kind").and_then(Value::as_str).unwrap_or("");
                let action = change.get("action").and_then(Value::as_str).unwrap_or("");
                let context = change.get("context").and_then(Value::as_str).unwrap_or("");
                let name = change.get("name").and_then(Value::as_str).unwrap_or("");
                let drift_params = params_map(&[
                    ("ws", &ws),
                    ("category", kind),
                    ("ctx", context),
                    ("name", name),
                    ("change", action),
                ]);
                self.run_mutation_script(
                    "?[workspace, category, context, name, change_type] <- [[$ws, $category, $ctx, $name, $change]] \
                     :put drift { workspace, category, context, name, change_type }",
                    drift_params,
                    format!("compute_drift insert {kind}:{name}"),
                )?;
            }

            let drift_ts_us = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_micros();
            let drift_ts_us = u128_to_i64_saturating(drift_ts_us);
            let mut meta_params = params_map(&[("ws", &ws)]);
            meta_params.insert("ts".into(), int_dv(drift_ts_us));
            self.run_mutation_script(
                "?[workspace, computed_at_us] <- [[$ws, $ts]] :put drift_meta { workspace => computed_at_us }",
                meta_params,
                format!("compute_drift update drift_meta for '{ws}'"),
            )?;

            self.invalidate_reasoning_claims_for_dependency(&ws, "drift")?;

            Ok(changes.len())
        })
    }

    /// Load current drift entries for a workspace.
    pub fn load_drift(
        &self,
        workspace_path: &str,
    ) -> Result<Vec<(String, String, String, String)>> {
        let ws = canonicalize_path(workspace_path);
        let params = params_map(&[("ws", &ws)]);
        let result = self
            .run_script(
                "?[category, context, name, change_type] := \
             *drift{workspace: $ws, category, context, name, change_type @ 'NOW'}",
                params,
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("load_drift: {:?}", e))?;
        Ok(result
            .rows
            .iter()
            .map(|r| (dv_str(&r[0]), dv_str(&r[1]), dv_str(&r[2]), dv_str(&r[3])))
            .collect())
    }

    /// Load the timestamp of the most recent persisted drift computation.
    pub fn load_drift_recomputed_at(&self, workspace_path: &str) -> Result<Option<i64>> {
        let ws = canonicalize_path(workspace_path);
        let params = params_map(&[("ws", &ws)]);
        let result = self
            .run_script(
                "?[computed_at_us] := *drift_meta{workspace: $ws, computed_at_us}",
                params,
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("load_drift_recomputed_at: {:?}", e))?;
        Ok(result.rows.first().map(|row| dv_i64(&row[0])))
    }

    /// Report the current truth-maintenance state for implemented graph and drift facts.
    pub fn truth_maintenance_report(&self, workspace_path: &str) -> Result<TruthMaintenanceReport> {
        let actual = self.load_actual(workspace_path)?;
        let actual_snapshot = self
            .list_snapshots(workspace_path, "actual")?
            .into_iter()
            .next();
        let drift_computed_at_us = self.load_drift_recomputed_at(workspace_path)?;
        let drift_entries = self.load_drift(workspace_path)?;

        let asserted =
            summarize_fact_snapshot("implemented", "actual", actual_snapshot, actual.as_ref());
        let scanned =
            summarize_fact_snapshot("scanned", "actual", actual_snapshot, actual.as_ref());

        let basis_timestamp_us = actual_snapshot;

        let drift_status = match (basis_timestamp_us, drift_computed_at_us) {
            (Some(basis_ts), Some(computed_at_us)) if computed_at_us >= basis_ts => "fresh",
            (Some(_), Some(_)) => "stale",
            _ => "unavailable",
        };

        let mut assumptions = Vec::new();
        if !asserted.available {
            assumptions.push(
                "No implemented architecture graph is stored; run a scan before requesting proofs."
                    .to_string(),
            );
        }
        if !scanned.available {
            assumptions.push(
                "No scanned implementation model is stored; proofs about actual code structure are incomplete."
                    .to_string(),
            );
        }
        match drift_status {
            "stale" => assumptions.push(
                "Persisted drift entries predate the latest asserted or scanned snapshot and may be stale."
                    .to_string(),
            ),
            "unavailable" if basis_timestamp_us.is_some() => assumptions.push(
                "Drift has not been recomputed for the current asserted/scanned basis."
                    .to_string(),
            ),
            _ => {}
        }

        Ok(TruthMaintenanceReport {
            asserted,
            scanned,
            drift: DriftFreshness {
                available: basis_timestamp_us.is_some() && drift_computed_at_us.is_some(),
                status: drift_status.to_string(),
                computed_at_us: drift_computed_at_us,
                basis_timestamp_us,
                entry_count: drift_entries.len(),
            },
            assumptions,
        })
    }

    fn clear_reasoning_claims(&self, workspace: &str) -> Result<()> {
        let scripts = [
            (
                "reasoning_derivation",
                "?[workspace, claim_id, idx] := *reasoning_derivation{workspace, claim_id, idx}, workspace = $ws :rm reasoning_derivation { workspace, claim_id, idx }",
            ),
            (
                "reasoning_assumption",
                "?[workspace, claim_id, idx] := *reasoning_assumption{workspace, claim_id, idx}, workspace = $ws :rm reasoning_assumption { workspace, claim_id, idx }",
            ),
            (
                "reasoning_support",
                "?[workspace, claim_id, idx] := *reasoning_support{workspace, claim_id, idx}, workspace = $ws :rm reasoning_support { workspace, claim_id, idx }",
            ),
            (
                "reasoning_dependency",
                "?[workspace, claim_id, idx] := *reasoning_dependency{workspace, claim_id, idx}, workspace = $ws :rm reasoning_dependency { workspace, claim_id, idx }",
            ),
            (
                "reasoning_justification",
                "?[workspace, claim_id, idx] := *reasoning_justification{workspace, claim_id, idx}, workspace = $ws :rm reasoning_justification { workspace, claim_id, idx }",
            ),
            (
                "reasoning_claim",
                "?[workspace, claim_id] := *reasoning_claim{workspace, claim_id}, workspace = $ws :rm reasoning_claim { workspace, claim_id }",
            ),
        ];

        for (relation, script) in scripts {
            self.run_mutation_script(
                script,
                params_map(&[("ws", workspace)]),
                format!("clear {relation} rows for '{workspace}'"),
            )?;
        }

        Ok(())
    }

    fn clear_reasoning_claim_ids(&self, workspace: &str, claim_ids: &[String]) -> Result<()> {
        if claim_ids.is_empty() {
            return Ok(());
        }

        let scripts = [
            (
                "reasoning_derivation",
                "?[workspace, claim_id, idx] := *reasoning_derivation{workspace, claim_id, idx}, workspace = $ws, claim_id = $claim_id :rm reasoning_derivation { workspace, claim_id, idx }",
            ),
            (
                "reasoning_assumption",
                "?[workspace, claim_id, idx] := *reasoning_assumption{workspace, claim_id, idx}, workspace = $ws, claim_id = $claim_id :rm reasoning_assumption { workspace, claim_id, idx }",
            ),
            (
                "reasoning_support",
                "?[workspace, claim_id, idx] := *reasoning_support{workspace, claim_id, idx}, workspace = $ws, claim_id = $claim_id :rm reasoning_support { workspace, claim_id, idx }",
            ),
            (
                "reasoning_dependency",
                "?[workspace, claim_id, idx] := *reasoning_dependency{workspace, claim_id, idx}, workspace = $ws, claim_id = $claim_id :rm reasoning_dependency { workspace, claim_id, idx }",
            ),
            (
                "reasoning_justification",
                "?[workspace, claim_id, idx] := *reasoning_justification{workspace, claim_id, idx}, workspace = $ws, claim_id = $claim_id :rm reasoning_justification { workspace, claim_id, idx }",
            ),
            (
                "reasoning_claim",
                "?[workspace, claim_id] := *reasoning_claim{workspace, claim_id}, workspace = $ws, claim_id = $claim_id :rm reasoning_claim { workspace, claim_id }",
            ),
        ];

        for claim_id in claim_ids {
            let params = params_map(&[("ws", workspace), ("claim_id", claim_id)]);
            for (relation, script) in scripts {
                self.run_mutation_script(
                    script,
                    params.clone(),
                    format!("clear {relation} rows for '{workspace}' claim '{claim_id}'"),
                )?;
            }
        }

        Ok(())
    }

    fn write_reasoning_claims(
        &self,
        workspace: &str,
        claims: &[PersistedReasoningClaim],
    ) -> Result<()> {
        for claim in claims {
            let payload_json =
                serde_json::to_string(&claim.payload).unwrap_or_else(|_| "{}".into());
            let mut claim_params = params_map(&[
                ("ws", workspace),
                ("claim_id", &claim.claim_id),
                ("claim_kind", &claim.claim_kind),
                ("subject", &claim.subject),
                ("status", &claim.status),
                ("summary", &claim.summary),
                ("payload_json", &payload_json),
                ("prov_source", &claim.provenance.source),
                ("prov_state", &claim.provenance.state),
            ]);
            claim_params.insert("stale".into(), cozo::DataValue::Bool(claim.stale));
            claim_params.insert("computed_at_us".into(), int_dv(claim.computed_at_us));
            self.run_mutation_script(
                "?[workspace, claim_id, claim_kind, subject, status, summary, payload_json, provenance_source, provenance_state, stale, computed_at_us] <- \
                 [[$ws, $claim_id, $claim_kind, $subject, $status, $summary, $payload_json, $prov_source, $prov_state, $stale, $computed_at_us]] \
                 :put reasoning_claim { workspace, claim_id => claim_kind, subject, status, summary, payload_json, provenance_source, provenance_state, stale, computed_at_us }",
                claim_params,
                format!("save reasoning claim '{}'", claim.claim_id),
            )?;

            for (idx, derivation) in claim.derivations.iter().enumerate() {
                let derived_from_json =
                    serde_json::to_string(&derivation.derived_from).unwrap_or_else(|_| "[]".into());
                let mut params = params_map(&[
                    ("ws", workspace),
                    ("claim_id", &claim.claim_id),
                    ("rule", &derivation.rule),
                    ("derived_from_json", &derived_from_json),
                ]);
                params.insert("idx".into(), int_dv(usize_to_i64(idx)));
                params.insert(
                    "witness_count".into(),
                    int_dv(usize_to_i64(derivation.witness_count)),
                );
                self.run_mutation_script(
                    "?[workspace, claim_id, idx, rule, derived_from_json, witness_count] <- \
                     [[$ws, $claim_id, $idx, $rule, $derived_from_json, $witness_count]] \
                     :put reasoning_derivation { workspace, claim_id, idx => rule, derived_from_json, witness_count }",
                    params,
                    format!("save reasoning derivation '{}' [{}]", claim.claim_id, idx),
                )?;
            }

            for (idx, assumption) in claim.assumptions.iter().enumerate() {
                let mut params = params_map(&[
                    ("ws", workspace),
                    ("claim_id", &claim.claim_id),
                    ("assumption_kind", &assumption.assumption_kind),
                    ("text", &assumption.text),
                ]);
                params.insert("idx".into(), int_dv(usize_to_i64(idx)));
                self.run_mutation_script(
                    "?[workspace, claim_id, idx, assumption_kind, text] <- \
                     [[$ws, $claim_id, $idx, $assumption_kind, $text]] \
                     :put reasoning_assumption { workspace, claim_id, idx => assumption_kind, text }",
                    params,
                    format!("save reasoning assumption '{}' [{}]", claim.claim_id, idx),
                )?;
            }

            for (idx, support) in claim.supports.iter().enumerate() {
                let detail_json =
                    serde_json::to_string(&support.detail).unwrap_or_else(|_| "{}".into());
                let mut params = params_map(&[
                    ("ws", workspace),
                    ("claim_id", &claim.claim_id),
                    ("support_kind", &support.support_kind),
                    ("summary", &support.summary),
                    ("detail_json", &detail_json),
                ]);
                params.insert("idx".into(), int_dv(usize_to_i64(idx)));
                self.run_mutation_script(
                    "?[workspace, claim_id, idx, support_kind, summary, detail_json] <- \
                     [[$ws, $claim_id, $idx, $support_kind, $summary, $detail_json]] \
                     :put reasoning_support { workspace, claim_id, idx => support_kind, summary, detail_json }",
                    params,
                    format!("save reasoning support '{}' [{}]", claim.claim_id, idx),
                )?;
            }

            for (idx, dependency) in claim.dependencies.iter().enumerate() {
                let mut params = params_map(&[
                    ("ws", workspace),
                    ("claim_id", &claim.claim_id),
                    ("dependency_kind", &dependency.dependency_kind),
                    ("dependency_state", &dependency.dependency_state),
                ]);
                params.insert("idx".into(), int_dv(usize_to_i64(idx)));
                params.insert(
                    "basis_timestamp_us".into(),
                    int_dv(dependency.basis_timestamp_us),
                );
                self.run_mutation_script(
                    "?[workspace, claim_id, idx, dependency_kind, dependency_state, basis_timestamp_us] <- \
                     [[$ws, $claim_id, $idx, $dependency_kind, $dependency_state, $basis_timestamp_us]] \
                     :put reasoning_dependency { workspace, claim_id, idx => dependency_kind, dependency_state, basis_timestamp_us }",
                    params,
                    format!("save reasoning dependency '{}' [{}]", claim.claim_id, idx),
                )?;
            }

            for (idx, justification) in claim.justifications.iter().enumerate() {
                let mut params = params_map(&[
                    ("ws", workspace),
                    ("claim_id", &claim.claim_id),
                    ("fact_kind", &justification.fact_kind),
                    ("fact_key", &justification.fact_key),
                    ("fact_state", &justification.fact_state),
                ]);
                params.insert("idx".into(), int_dv(usize_to_i64(idx)));
                params.insert(
                    "basis_timestamp_us".into(),
                    int_dv(justification.basis_timestamp_us),
                );
                self.run_mutation_script(
                    "?[workspace, claim_id, idx, fact_kind, fact_key, fact_state, basis_timestamp_us] <- \
                     [[$ws, $claim_id, $idx, $fact_kind, $fact_key, $fact_state, $basis_timestamp_us]] \
                     :put reasoning_justification { workspace, claim_id, idx => fact_kind, fact_key, fact_state, basis_timestamp_us }",
                    params,
                    format!("save reasoning justification '{}' [{}]", claim.claim_id, idx),
                )?;
            }
        }

        Ok(())
    }

    pub fn save_reasoning_claims(
        &self,
        workspace_path: &str,
        claims: &[PersistedReasoningClaim],
    ) -> Result<()> {
        let ws = canonicalize_path(workspace_path);
        self.clear_reasoning_claims(&ws)?;

        self.write_reasoning_claims(&ws, claims)
    }

    pub fn upsert_reasoning_claims(
        &self,
        workspace_path: &str,
        claims: &[PersistedReasoningClaim],
    ) -> Result<()> {
        let ws = canonicalize_path(workspace_path);
        let claim_ids: Vec<String> = claims.iter().map(|claim| claim.claim_id.clone()).collect();
        self.clear_reasoning_claim_ids(&ws, &claim_ids)?;

        self.write_reasoning_claims(&ws, claims)
    }

    pub fn load_reasoning_claims(
        &self,
        workspace_path: &str,
    ) -> Result<Vec<PersistedReasoningClaim>> {
        let ws = canonicalize_path(workspace_path);
        let result = self
            .run_script(
                "?[claim_id] := *reasoning_claim{workspace: $ws, claim_id} :sort claim_id",
                params_map(&[("ws", &ws)]),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("load_reasoning_claims: {:?}", e))?;

        let mut claims = Vec::with_capacity(result.rows.len());
        for row in &result.rows {
            if let Some(claim) = self.load_reasoning_claim(&ws, &dv_str(&row[0]))? {
                claims.push(claim);
            }
        }
        Ok(claims)
    }

    pub fn load_reasoning_claim(
        &self,
        workspace_path: &str,
        claim_id: &str,
    ) -> Result<Option<PersistedReasoningClaim>> {
        let ws = canonicalize_path(workspace_path);
        let header = self
            .run_script(
                "?[claim_kind, subject, status, summary, payload_json, provenance_source, provenance_state, stale, computed_at_us] := \
                 *reasoning_claim{workspace: $ws, claim_id: $claim_id, claim_kind, subject, status, summary, payload_json, provenance_source, provenance_state, stale, computed_at_us}",
                params_map(&[("ws", &ws), ("claim_id", claim_id)]),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("load_reasoning_claim '{}': {:?}", claim_id, e))?;

        let Some(row) = header.rows.first() else {
            return Ok(None);
        };

        let derivations = self
            .run_script(
                "?[idx, rule, derived_from_json, witness_count] := *reasoning_derivation{workspace: $ws, claim_id: $claim_id, idx, rule, derived_from_json, witness_count} :sort idx",
                params_map(&[("ws", &ws), ("claim_id", claim_id)]),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("load_reasoning_derivation '{}': {:?}", claim_id, e))?
            .rows
            .iter()
            .map(|row| ReasoningDerivation {
                rule: dv_str(&row[1]),
                derived_from: serde_json::from_str::<Vec<String>>(&dv_str(&row[2]))
                    .unwrap_or_default(),
                witness_count: i64_to_usize_saturating(dv_i64(&row[3])),
            })
            .collect();

        let assumptions = self
            .run_script(
                "?[idx, assumption_kind, text] := *reasoning_assumption{workspace: $ws, claim_id: $claim_id, idx, assumption_kind, text} :sort idx",
                params_map(&[("ws", &ws), ("claim_id", claim_id)]),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("load_reasoning_assumption '{}': {:?}", claim_id, e))?
            .rows
            .iter()
            .map(|row| ReasoningAssumption {
                assumption_kind: dv_str(&row[1]),
                text: dv_str(&row[2]),
            })
            .collect();

        let supports = self
            .run_script(
                "?[idx, support_kind, summary, detail_json] := *reasoning_support{workspace: $ws, claim_id: $claim_id, idx, support_kind, summary, detail_json} :sort idx",
                params_map(&[("ws", &ws), ("claim_id", claim_id)]),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("load_reasoning_support '{}': {:?}", claim_id, e))?
            .rows
            .iter()
            .map(|row| ReasoningSupportEdge {
                support_kind: dv_str(&row[1]),
                summary: dv_str(&row[2]),
                detail: serde_json::from_str::<Value>(&dv_str(&row[3]))
                    .unwrap_or_else(|_| json!({})),
            })
            .collect();

        let dependencies = self
            .run_script(
                "?[idx, dependency_kind, dependency_state, basis_timestamp_us] := *reasoning_dependency{workspace: $ws, claim_id: $claim_id, idx, dependency_kind, dependency_state, basis_timestamp_us} :sort idx",
                params_map(&[("ws", &ws), ("claim_id", claim_id)]),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("load_reasoning_dependency '{}': {:?}", claim_id, e))?
            .rows
            .iter()
            .map(|row| ReasoningDependency {
                dependency_kind: dv_str(&row[1]),
                dependency_state: dv_str(&row[2]),
                basis_timestamp_us: dv_i64(&row[3]),
            })
            .collect();

        let justifications = self
            .run_script(
                "?[idx, fact_kind, fact_key, fact_state, basis_timestamp_us] := *reasoning_justification{workspace: $ws, claim_id: $claim_id, idx, fact_kind, fact_key, fact_state, basis_timestamp_us} :sort idx",
                params_map(&[("ws", &ws), ("claim_id", claim_id)]),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("load_reasoning_justification '{}': {:?}", claim_id, e))?
            .rows
            .iter()
            .map(|row| ReasoningJustification {
                fact_kind: dv_str(&row[1]),
                fact_key: dv_str(&row[2]),
                fact_state: dv_str(&row[3]),
                basis_timestamp_us: dv_i64(&row[4]),
            })
            .collect();

        Ok(Some(PersistedReasoningClaim {
            claim_id: claim_id.to_string(),
            claim_kind: dv_str(&row[0]),
            subject: dv_str(&row[1]),
            status: dv_str(&row[2]),
            summary: dv_str(&row[3]),
            payload: serde_json::from_str::<Value>(&dv_str(&row[4])).unwrap_or_else(|_| json!({})),
            provenance: ReasoningProvenance {
                source: dv_str(&row[5]),
                state: dv_str(&row[6]),
            },
            stale: matches!(&row[7], cozo::DataValue::Bool(true)),
            computed_at_us: dv_i64(&row[8]),
            derivations,
            assumptions,
            supports,
            dependencies,
            justifications,
        }))
    }

    pub fn load_stale_reasoning_claims(
        &self,
        workspace_path: &str,
    ) -> Result<Vec<PersistedReasoningClaim>> {
        let ws = canonicalize_path(workspace_path);
        let result = self
            .run_script(
                "?[claim_id] := *reasoning_claim{workspace: $ws, claim_id, claim_kind, subject, status, summary, payload_json, provenance_source, provenance_state, stale: true, computed_at_us} :sort claim_id",
                params_map(&[("ws", &ws)]),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("load_stale_reasoning_claims: {:?}", e))?;

        let mut claims = Vec::with_capacity(result.rows.len());
        for row in &result.rows {
            if let Some(claim) = self.load_reasoning_claim(&ws, &dv_str(&row[0]))? {
                claims.push(claim);
            }
        }
        Ok(claims)
    }

    pub fn load_stale_reasoning_claims_for_dependency(
        &self,
        workspace_path: &str,
        dependency_state: &str,
    ) -> Result<Vec<PersistedReasoningClaim>> {
        let ws = canonicalize_path(workspace_path);
        let result = self
            .run_script(
                "?[claim_id] := \
                 *reasoning_dependency{workspace: $ws, claim_id, idx, dependency_kind, dependency_state: $dependency_state, basis_timestamp_us}, \
                 *reasoning_claim{workspace: $ws, claim_id, claim_kind, subject, status, summary, payload_json, provenance_source, provenance_state, stale: true, computed_at_us} \
                 :sort claim_id",
                params_map(&[("ws", &ws), ("dependency_state", dependency_state)]),
                ScriptMutability::Immutable,
            )
            .map_err(|e| {
                anyhow::anyhow!(
                    "load_stale_reasoning_claims_for_dependency '{}': {:?}",
                    dependency_state,
                    e
                )
            })?;

        let mut claim_ids = BTreeSet::new();
        let mut claims = Vec::new();
        for row in &result.rows {
            let claim_id = dv_str(&row[0]);
            if claim_ids.insert(claim_id.clone())
                && let Some(claim) = self.load_reasoning_claim(&ws, &claim_id)?
            {
                claims.push(claim);
            }
        }

        Ok(claims)
    }

    pub fn invalidate_reasoning_claims_for_dependency(
        &self,
        workspace_path: &str,
        dependency_state: &str,
    ) -> Result<usize> {
        let ws = canonicalize_path(workspace_path);
        let result = self
            .run_script(
                "?[claim_id] := *reasoning_dependency{workspace: $ws, claim_id, idx, dependency_kind, dependency_state: $dependency_state, basis_timestamp_us} :sort claim_id",
                params_map(&[("ws", &ws), ("dependency_state", dependency_state)]),
                ScriptMutability::Immutable,
            )
            .map_err(|e| {
                anyhow::anyhow!(
                    "invalidate_reasoning_claims_for_dependency '{}': {:?}",
                    dependency_state,
                    e
                )
            })?;

        for row in &result.rows {
            let claim_id = dv_str(&row[0]);
            self.run_mutation_script(
                "current[claim_kind, subject, status, summary, payload_json, provenance_source, provenance_state, computed_at_us] := \
                 *reasoning_claim{workspace: $ws, claim_id: $claim_id, claim_kind, subject, status, summary, payload_json, provenance_source, provenance_state, stale: current_stale, computed_at_us} \
                 ?[workspace, claim_id, claim_kind, subject, status, summary, payload_json, provenance_source, provenance_state, stale, computed_at_us] := \
                 current[claim_kind, subject, status, summary, payload_json, provenance_source, provenance_state, computed_at_us], workspace = $ws, claim_id = $claim_id, stale = true \
                 :put reasoning_claim { workspace, claim_id => claim_kind, subject, status, summary, payload_json, provenance_source, provenance_state, stale, computed_at_us }",
                params_map(&[("ws", &ws), ("claim_id", &claim_id)]),
                format!(
                    "mark reasoning claim '{}' stale for dependency '{}'",
                    claim_id,
                    dependency_state
                ),
            )?;
        }

        Ok(result.rows.len())
    }

    pub fn invalidate_reasoning_claims_for_facts(
        &self,
        workspace_path: &str,
        facts: &[ReasoningFactRef],
    ) -> Result<usize> {
        let ws = canonicalize_path(workspace_path);
        let mut claim_ids = BTreeSet::new();

        for fact in facts {
            let result = self
                .run_script(
                    "?[claim_id, fact_key] := *reasoning_justification{workspace: $ws, claim_id, idx, fact_kind: $fact_kind, fact_key, fact_state: $fact_state, basis_timestamp_us}",
                    params_map(&[
                        ("ws", &ws),
                        ("fact_kind", &fact.fact_kind),
                        ("fact_state", &fact.fact_state),
                    ]),
                    ScriptMutability::Immutable,
                )
                .map_err(|e| anyhow::anyhow!("invalidate_reasoning_claims_for_facts '{:?}': {:?}", fact, e))?;

            for row in &result.rows {
                let claim_id = dv_str(&row[0]);
                let stored_key = dv_str(&row[1]);
                if fact.fact_key == "*" || stored_key == "*" || stored_key == fact.fact_key {
                    claim_ids.insert(claim_id);
                }
            }
        }

        for claim_id in &claim_ids {
            self.run_mutation_script(
                "current[claim_kind, subject, status, summary, payload_json, provenance_source, provenance_state, computed_at_us] := \
                 *reasoning_claim{workspace: $ws, claim_id: $claim_id, claim_kind, subject, status, summary, payload_json, provenance_source, provenance_state, stale: current_stale, computed_at_us} \
                 ?[workspace, claim_id, claim_kind, subject, status, summary, payload_json, provenance_source, provenance_state, stale, computed_at_us] := \
                 current[claim_kind, subject, status, summary, payload_json, provenance_source, provenance_state, computed_at_us], workspace = $ws, claim_id = $claim_id, stale = true \
                 :put reasoning_claim { workspace, claim_id => claim_kind, subject, status, summary, payload_json, provenance_source, provenance_state, stale, computed_at_us }",
                params_map(&[("ws", &ws), ("claim_id", claim_id)]),
                format!("mark reasoning claim '{}' stale for fact invalidation", claim_id),
            )?;
        }

        Ok(claim_ids.len())
    }

    /// List distinct save timestamps for a workspace+state, derived from
    /// the `snapshot_log` relation. Returns microsecond timestamps in
    /// descending order (most recent first).
    pub fn list_snapshots(&self, workspace_path: &str, state: &str) -> Result<Vec<i64>> {
        let ws = canonicalize_path(workspace_path);
        let state = canonical_model_state(state);
        let params = params_map(&[("ws", &ws), ("st", state)]);
        let result = self
            .run_script(
                "?[ts] := *snapshot_log{workspace: $ws, state: $st, timestamp_us: ts} \
             :sort -ts",
                params,
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("list_snapshots: {:?}", e))?;
        Ok(result.rows.iter().map(|r| dv_i64(&r[0])).collect())
    }

    /// Compare two Validity timestamps and return the diff of entities present
    /// at `ts_new` but not at `ts_old` (added) and vice versa (removed).
    /// Timestamps are microsecond epoch values from `list_snapshots`.
    pub fn diff_snapshots(
        &self,
        workspace_path: &str,
        state: &str,
        ts_old: i64,
        ts_new: i64,
    ) -> Result<serde_json::Value> {
        let ws = canonicalize_path(workspace_path);
        let state = canonical_model_state(state);
        let mut params = params_map(&[("ws", &ws), ("st", state)]);
        params.insert("ts_old".into(), cozo::DataValue::from(ts_old));
        params.insert("ts_new".into(), cozo::DataValue::from(ts_new));

        // Use parameterized @ for point-in-time queries, then diff via derived rules.
        let script = "\
            ctx_new[name] := *context{workspace: $ws, name, state: $st @ $ts_new} \
            ctx_old[name] := *context{workspace: $ws, name, state: $st @ $ts_old} \
            ent_new[ctx, name] := *entity{workspace: $ws, context: ctx, name, state: $st @ $ts_new} \
            ent_old[ctx, name] := *entity{workspace: $ws, context: ctx, name, state: $st @ $ts_old} \
            svc_new[ctx, name] := *service{workspace: $ws, context: ctx, name, state: $st @ $ts_new} \
            svc_old[ctx, name] := *service{workspace: $ws, context: ctx, name, state: $st @ $ts_old} \
            evt_new[ctx, name] := *event{workspace: $ws, context: ctx, name, state: $st @ $ts_new} \
            evt_old[ctx, name] := *event{workspace: $ws, context: ctx, name, state: $st @ $ts_old} \
            vo_new[ctx, name] := *value_object{workspace: $ws, context: ctx, name, state: $st @ $ts_new} \
            vo_old[ctx, name] := *value_object{workspace: $ws, context: ctx, name, state: $st @ $ts_old} \
            repo_new[ctx, name] := *repository{workspace: $ws, context: ctx, name, state: $st @ $ts_new} \
            repo_old[ctx, name] := *repository{workspace: $ws, context: ctx, name, state: $st @ $ts_old} \
            mod_new[ctx, name] := *module{workspace: $ws, context: ctx, name, state: $st @ $ts_new} \
            mod_old[ctx, name] := *module{workspace: $ws, context: ctx, name, state: $st @ $ts_old} \
            fld_new[ctx, ok, ow, name] := *field{workspace: $ws, context: ctx, owner_kind: ok, owner: ow, name, state: $st @ $ts_new} \
            fld_old[ctx, ok, ow, name] := *field{workspace: $ws, context: ctx, owner_kind: ok, owner: ow, name, state: $st @ $ts_old} \
            mth_new[ctx, ok, ow, name] := *method{workspace: $ws, context: ctx, owner_kind: ok, owner: ow, name, state: $st @ $ts_new} \
            mth_old[ctx, ok, ow, name] := *method{workspace: $ws, context: ctx, owner_kind: ok, owner: ow, name, state: $st @ $ts_old} \
            inv_new[ctx, ow, text] := *invariant{workspace: $ws, context: ctx, entity: ow, text, state: $st @ $ts_new} \
            inv_old[ctx, ow, text] := *invariant{workspace: $ws, context: ctx, entity: ow, text, state: $st @ $ts_old} \
            ?[kind, action, ctx, name, owner_kind, owner] := ctx_new[name], not ctx_old[name], kind = 'context', action = 'add', ctx = '', owner_kind = '', owner = '' \
            ?[kind, action, ctx, name, owner_kind, owner] := ctx_old[name], not ctx_new[name], kind = 'context', action = 'remove', ctx = '', owner_kind = '', owner = '' \
            ?[kind, action, ctx, name, owner_kind, owner] := ent_new[ctx, name], not ent_old[ctx, name], kind = 'entity', action = 'add', owner_kind = '', owner = '' \
            ?[kind, action, ctx, name, owner_kind, owner] := ent_old[ctx, name], not ent_new[ctx, name], kind = 'entity', action = 'remove', owner_kind = '', owner = '' \
            ?[kind, action, ctx, name, owner_kind, owner] := svc_new[ctx, name], not svc_old[ctx, name], kind = 'service', action = 'add', owner_kind = '', owner = '' \
            ?[kind, action, ctx, name, owner_kind, owner] := svc_old[ctx, name], not svc_new[ctx, name], kind = 'service', action = 'remove', owner_kind = '', owner = '' \
            ?[kind, action, ctx, name, owner_kind, owner] := evt_new[ctx, name], not evt_old[ctx, name], kind = 'event', action = 'add', owner_kind = '', owner = '' \
            ?[kind, action, ctx, name, owner_kind, owner] := evt_old[ctx, name], not evt_new[ctx, name], kind = 'event', action = 'remove', owner_kind = '', owner = '' \
            ?[kind, action, ctx, name, owner_kind, owner] := vo_new[ctx, name], not vo_old[ctx, name], kind = 'value_object', action = 'add', owner_kind = '', owner = '' \
            ?[kind, action, ctx, name, owner_kind, owner] := vo_old[ctx, name], not vo_new[ctx, name], kind = 'value_object', action = 'remove', owner_kind = '', owner = '' \
            ?[kind, action, ctx, name, owner_kind, owner] := repo_new[ctx, name], not repo_old[ctx, name], kind = 'repository', action = 'add', owner_kind = '', owner = '' \
            ?[kind, action, ctx, name, owner_kind, owner] := repo_old[ctx, name], not repo_new[ctx, name], kind = 'repository', action = 'remove', owner_kind = '', owner = '' \
            ?[kind, action, ctx, name, owner_kind, owner] := mod_new[ctx, name], not mod_old[ctx, name], kind = 'module', action = 'add', owner_kind = '', owner = '' \
            ?[kind, action, ctx, name, owner_kind, owner] := mod_old[ctx, name], not mod_new[ctx, name], kind = 'module', action = 'remove', owner_kind = '', owner = '' \
            ?[kind, action, ctx, name, owner_kind, owner] := fld_new[ctx, owner_kind, owner, name], not fld_old[ctx, owner_kind, owner, name], kind = 'field', action = 'add' \
            ?[kind, action, ctx, name, owner_kind, owner] := fld_old[ctx, owner_kind, owner, name], not fld_new[ctx, owner_kind, owner, name], kind = 'field', action = 'remove' \
            ?[kind, action, ctx, name, owner_kind, owner] := mth_new[ctx, owner_kind, owner, name], not mth_old[ctx, owner_kind, owner, name], kind = 'method', action = 'add' \
            ?[kind, action, ctx, name, owner_kind, owner] := mth_old[ctx, owner_kind, owner, name], not mth_new[ctx, owner_kind, owner, name], kind = 'method', action = 'remove' \
            ?[kind, action, ctx, name, owner_kind, owner] := inv_new[ctx, owner, name], not inv_old[ctx, owner, name], kind = 'invariant', action = 'add', owner_kind = 'entity' \
            ?[kind, action, ctx, name, owner_kind, owner] := inv_old[ctx, owner, name], not inv_new[ctx, owner, name], kind = 'invariant', action = 'remove', owner_kind = 'entity'";

        let result = self
            .run_script(script, params, ScriptMutability::Immutable)
            .map_err(|e| anyhow::anyhow!("diff_snapshots: {:?}", e))?;

        let changes: Vec<serde_json::Value> = result
            .rows
            .iter()
            .map(|r| {
                let mut entry = json!({
                    "kind": dv_str(&r[0]),
                    "action": dv_str(&r[1]),
                    "name": dv_str(&r[3]),
                });
                let ctx = dv_str(&r[2]);
                if !ctx.is_empty() {
                    entry["context"] = json!(ctx);
                }
                let owner_kind = dv_str(&r[4]);
                if !owner_kind.is_empty() {
                    entry["owner_kind"] = json!(owner_kind);
                    entry["owner"] = json!(dv_str(&r[5]));
                }
                entry
            })
            .collect();

        let added: Vec<_> = changes
            .iter()
            .filter(|c| c["action"] == "add")
            .cloned()
            .collect();
        let removed: Vec<_> = changes
            .iter()
            .filter(|c| c["action"] == "remove")
            .cloned()
            .collect();

        Ok(json!({
            "ts_old": ts_old,
            "ts_new": ts_new,
            "state": state,
            "summary": {
                "total_changes": changes.len(),
                "additions": added.len(),
                "removals": removed.len(),
            },
            "added": added,
            "removed": removed,
        }))
    }

    // ── Live AST Bridge ───────────────────────────────────────────────────

    /// Project live AST imports into the ephemeral `live_import` table,
    /// then cross-reference against the domain model to detect violations.
    pub fn check_live_dependencies(
        &self,
        workspace_path: &str,
        live_deps: &[crate::domain::analyze::LiveDependency],
    ) -> Result<Vec<crate::domain::analyze::LiveDependency>> {
        let ws = canonicalize_path(workspace_path);

        // 1. Clear previous live_import rows
        let clear_params = params_map(&[("ws", &ws)]);
        let _ = self.run_script(
            "?[workspace, from_file, to_module] := *live_import{workspace: $ws, from_file, to_module} :rm live_import { workspace, from_file, to_module }",
            clear_params,
            ScriptMutability::Mutable,
        );

        // 2. Insert current live imports
        if !live_deps.is_empty() {
            let mut values = Vec::new();
            for dep in live_deps {
                values.push(cozo::DataValue::List(vec![
                    cozo::DataValue::Str(ws.clone().into()),
                    cozo::DataValue::Str(dep.from_file.clone().into()),
                    cozo::DataValue::Str(dep.to_module.clone().into()),
                ]));
            }
            let params = BTreeMap::from([("rows".to_string(), cozo::DataValue::List(values))]);
            self.run_script(
                "?[workspace, from_file, to_module] <- $rows \
                     :put live_import { workspace, from_file, to_module }",
                params,
                ScriptMutability::Mutable,
            )
            .map_err(|e| anyhow::anyhow!("insert live_imports: {:?}", e))?;
        }

        // 3. Cross-reference against modeled contexts (desired state)
        let query_params = params_map(&[("ws", &ws)]);
        let result = self
            .run_script(
                "modeled[m] := *context{workspace: $ws, module_path: m, state: 'actual' @ 'NOW'}, m != '' \
                 ?[from_file, to_module] := *live_import{workspace: $ws, from_file, to_module}, \
                     not modeled[to_module]",
                query_params,
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("check_live_dependencies: {:?}", e))?;

        Ok(result
            .rows
            .iter()
            .map(|r| crate::domain::analyze::LiveDependency {
                from_file: dv_str(&r[0]),
                to_module: dv_str(&r[1]),
            })
            .collect())
    }

    // ── Datalog Query Runners ─────────────────────────────────────────────

    /// Run an arbitrary Datalog query with `$ws` parameter.
    pub fn run_datalog(&self, script: &str, workspace: &str) -> Result<Vec<Vec<String>>> {
        let ws = canonicalize_path(workspace);
        let params = params_map(&[("ws", &ws)]);
        let script = normalize_query_state_aliases(script);
        let result = self
            .run_script(&script, params, ScriptMutability::Immutable)
            .map_err(|e| anyhow::anyhow!("Datalog query failed: {:?}", e))?;
        Ok(result
            .rows
            .iter()
            .map(|row| row.iter().map(dv_str).collect())
            .collect())
    }

    /// Run an arbitrary Datalog query, returning headers + rows.
    pub fn run_datalog_full(
        &self,
        script: &str,
        workspace: &str,
    ) -> Result<(Vec<String>, Vec<Vec<String>>)> {
        let ws = canonicalize_path(workspace);
        let params = params_map(&[("ws", &ws)]);
        let script = normalize_query_state_aliases(script);
        let result = self
            .run_script(&script, params, ScriptMutability::Immutable)
            .map_err(|e| anyhow::anyhow!("Datalog query failed: {:?}", e))?;
        let headers = result.headers.iter().map(|h| h.to_string()).collect();
        let rows = result
            .rows
            .iter()
            .map(|row| row.iter().map(dv_str).collect())
            .collect();
        Ok((headers, rows))
    }

    // ── Datalog Inference Queries (always query desired state) ─────────────

    pub fn transitive_deps(&self, workspace: &str, context: &str) -> Result<Vec<String>> {
        let params = params_map(&[("ws", workspace), ("ctx", context)]);
        let result = self
            .run_script(
                "transitive[a, c] := *context_dep{workspace: $ws, from_ctx: a, to_ctx: c, state: 'actual' @ 'NOW'} \
                 transitive[a, c] := transitive[a, b], *context_dep{workspace: $ws, from_ctx: b, to_ctx: c, state: 'actual' @ 'NOW'} \
                 ?[dep] := transitive[$ctx, dep]",
                params,
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("transitive_deps: {:?}", e))?;
        Ok(result.rows.iter().map(|r| dv_str(&r[0])).collect())
    }

    pub fn circular_deps(&self, workspace: &str) -> Result<Vec<(String, String)>> {
        let params = params_map(&[("ws", workspace)]);
        let result = self
            .run_script(
                "transitive[a, c] := *context_dep{workspace: $ws, from_ctx: a, to_ctx: c, state: 'actual' @ 'NOW'} \
                 transitive[a, c] := transitive[a, b], *context_dep{workspace: $ws, from_ctx: b, to_ctx: c, state: 'actual' @ 'NOW'} \
                 ?[a, b] := transitive[a, b], transitive[b, a]",
                params,
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("circular_deps: {:?}", e))?;
        Ok(result
            .rows
            .iter()
            .map(|r| (dv_str(&r[0]), dv_str(&r[1])))
            .collect())
    }

    /// Detect module-level import cycles from the syntactic `import_edge` facts.
    ///
    /// The context-level `circular_deps` only sees cycles between bounded
    /// contexts; a cycle between two modules *inside* one context (e.g.
    /// `dht` ⇄ `fabric`) is invisible there. This resolves each import's
    /// `to_module` use-path to an internal module and returns strongly-connected
    /// components (size > 1) of the resulting module graph. Heuristics: only
    /// `crate::`/`super::`/`self::`-qualified paths count as internal, and
    /// parent/child module pairs are excluded (structural, not dependency
    /// cycles). It runs over actual (implemented) imports, not the desired model.
    pub fn module_cycles(&self, workspace: &str) -> Result<Vec<Vec<String>>> {
        let params = params_map(&[("ws", workspace)]);
        let files = self
            .run_script(
                "?[path] := *source_file{workspace: $ws, path @ 'NOW'}",
                params.clone(),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("module_cycles files: {:?}", e))?;
        let imports = self
            .run_script(
                "?[from_file, to_module] := *import_edge{workspace: $ws, from_file, to_module @ 'NOW'}",
                params,
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("module_cycles imports: {:?}", e))?;

        // Map every internal source file to its module path and collect the set
        // of known internal modules (used for longest-prefix resolution).
        let mut file_to_mod: HashMap<String, String> = HashMap::new();
        for row in &files.rows {
            let path = dv_str(&row[0]);
            let m = file_module_path(&path);
            file_to_mod.insert(path, m);
        }
        for row in &imports.rows {
            let path = dv_str(&row[0]);
            file_to_mod
                .entry(path.clone())
                .or_insert_with(|| file_module_path(&path));
        }
        let known: BTreeSet<String> = file_to_mod.values().cloned().collect();

        // Build the directed module graph from cross-branch internal imports.
        let mut node_set: BTreeSet<String> = BTreeSet::new();
        let mut raw_edges: BTreeSet<(String, String)> = BTreeSet::new();
        for row in &imports.rows {
            let from_file = dv_str(&row[0]);
            let to_module = dv_str(&row[1]);
            let Some(from_mod) = file_to_mod.get(&from_file) else {
                continue;
            };
            let Some(to_mod) = resolve_internal_module(&to_module, from_mod, &known) else {
                continue;
            };
            if &to_mod == from_mod
                || is_ancestor(from_mod, &to_mod)
                || is_ancestor(&to_mod, from_mod)
            {
                continue;
            }
            node_set.insert(from_mod.clone());
            node_set.insert(to_mod.clone());
            raw_edges.insert((from_mod.clone(), to_mod));
        }

        let nodes: Vec<String> = node_set.into_iter().collect();
        let n = nodes.len();
        if n == 0 {
            return Ok(Vec::new());
        }
        let idx: HashMap<&str, usize> = nodes
            .iter()
            .enumerate()
            .map(|(i, s)| (s.as_str(), i))
            .collect();
        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (from, to) in &raw_edges {
            if let (Some(&fi), Some(&ti)) = (idx.get(from.as_str()), idx.get(to.as_str())) {
                adj[fi].push(ti);
            }
        }
        // Reachability per node (BFS), then group strongly-connected components.
        let mut reach: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n];
        for s in 0..n {
            let mut seen: BTreeSet<usize> = BTreeSet::new();
            let mut queue: VecDeque<usize> = adj[s].iter().copied().collect();
            while let Some(v) = queue.pop_front() {
                if seen.insert(v) {
                    for &w in &adj[v] {
                        if !seen.contains(&w) {
                            queue.push_back(w);
                        }
                    }
                }
            }
            reach[s] = seen;
        }
        let mut visited = vec![false; n];
        let mut cycles: Vec<Vec<String>> = Vec::new();
        for u in 0..n {
            if visited[u] {
                continue;
            }
            visited[u] = true;
            let mut group = vec![u];
            for v in (u + 1)..n {
                if !visited[v] && reach[u].contains(&v) && reach[v].contains(&u) {
                    visited[v] = true;
                    group.push(v);
                }
            }
            if group.len() > 1 {
                let mut names: Vec<String> = group.iter().map(|&i| nodes[i].clone()).collect();
                names.sort();
                cycles.push(names);
            }
        }
        cycles.sort();
        Ok(cycles)
    }

    pub fn layer_violations(&self, workspace: &str) -> Result<Vec<(String, String, String)>> {
        let params = params_map(&[("ws", workspace)]);
        let result = self
            .run_script(
                "?[context, service, dep] := \
                    *service{workspace: $ws, context, name: service, kind: 'domain', state: 'actual' @ 'NOW'}, \
                    *service_dep{workspace: $ws, context, service, dep, state: 'actual' @ 'NOW'}, \
                    *service{workspace: $ws, context, name: dep, kind: 'infrastructure', state: 'actual' @ 'NOW'}",
                params,
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("layer_violations: {:?}", e))?;
        Ok(result
            .rows
            .iter()
            .map(|r| (dv_str(&r[0]), dv_str(&r[1]), dv_str(&r[2])))
            .collect())
    }

    // ── Architecture Constraint Operations ─────────────────────────────────

    /// Refresh in-memory architecture constraints for a workspace.
    ///
    /// Defaults and convention-inferred layers are recomputed in memory. Runtime
    /// overrides are preserved for the active store session and are never written
    /// into the scanned repository.
    pub fn refresh_runtime_constraints(&self, workspace: &str) -> Result<bool> {
        let ws = canonicalize_path(workspace);
        let before = self.policy_snapshot(&ws)?;

        self.seed_default_constraints(&ws)?;
        if let Some(model) = self.load_actual(&ws)? {
            self.apply_inferred_layers(&ws, &model)?;
        }

        let after = self.policy_snapshot(&ws)?;
        let changed = before != after;
        if changed {
            self.invalidate_reasoning_claims_for_facts(
                &ws,
                &[
                    ReasoningFactRef {
                        fact_kind: "layer_assignment".into(),
                        fact_key: "*".into(),
                        fact_state: "actual".into(),
                    },
                    ReasoningFactRef {
                        fact_kind: "dependency_constraint".into(),
                        fact_key: "*".into(),
                        fact_state: "actual".into(),
                    },
                ],
            )?;
            self.invalidate_reasoning_claims_for_dependency(&ws, "actual")?;
        }

        Ok(changed)
    }

    fn policy_snapshot(&self, workspace: &str) -> Result<PolicySnapshot> {
        Ok((
            self.list_layer_assignments(workspace)?
                .into_iter()
                .collect(),
            self.list_dependency_constraints(workspace)?
                .into_iter()
                .collect(),
        ))
    }

    /// Assign a bounded context to an architectural layer.
    pub fn upsert_layer_assignment(
        &self,
        workspace: &str,
        context: &str,
        layer: &str,
    ) -> Result<()> {
        let ws = canonicalize_path(workspace);
        self.upsert_layer_assignment_in_memory(&ws, context, layer)?;
        Ok(())
    }

    fn upsert_layer_assignment_in_memory(
        &self,
        workspace: &str,
        context: &str,
        layer: &str,
    ) -> Result<()> {
        let ws = canonicalize_path(workspace);
        let params = params_map(&[("ws", &ws), ("ctx", context), ("layer", layer)]);
        self.run_script(
            "?[workspace, context, layer] <- [[$ws, $ctx, $layer]] \
                 :put layer_assignment { workspace, context => layer }",
            params,
            ScriptMutability::Mutable,
        )
        .map_err(|e| anyhow::anyhow!("upsert_layer_assignment: {:?}", e))?;
        Ok(())
    }

    /// Remove a layer assignment for a bounded context.
    pub fn remove_layer_assignment(&self, workspace: &str, context: &str) -> Result<bool> {
        let ws = canonicalize_path(workspace);
        let params = params_map(&[("ws", &ws), ("ctx", context)]);
        let existing = self
            .run_script(
                "?[workspace, context] := *layer_assignment{workspace: $ws, context: $ctx} :rm layer_assignment { workspace, context }",
                params,
                ScriptMutability::Mutable,
            )
            .map_err(|e| anyhow::anyhow!("remove_layer_assignment: {:?}", e))?;
        Ok(!existing.rows.is_empty())
    }

    /// Add a dependency constraint between layers or contexts.
    /// `constraint_kind` is `"layer"` or `"context"`.
    /// `rule` is `"forbidden"` or `"allowed"`.
    pub fn upsert_dependency_constraint(
        &self,
        workspace: &str,
        constraint_kind: &str,
        source: &str,
        target: &str,
        rule: &str,
    ) -> Result<()> {
        let ws = canonicalize_path(workspace);
        self.upsert_dependency_constraint_in_memory(&ws, constraint_kind, source, target, rule)?;
        Ok(())
    }

    fn upsert_dependency_constraint_in_memory(
        &self,
        workspace: &str,
        constraint_kind: &str,
        source: &str,
        target: &str,
        rule: &str,
    ) -> Result<()> {
        let ws = canonicalize_path(workspace);
        let params = params_map(&[
            ("ws", &ws),
            ("kind", constraint_kind),
            ("src", source),
            ("tgt", target),
            ("rule", rule),
        ]);
        self
            .run_script(
                "?[workspace, constraint_kind, source, target, rule] <- [[$ws, $kind, $src, $tgt, $rule]] \
                 :put dependency_constraint { workspace, constraint_kind, source, target => rule }",
                params,
                ScriptMutability::Mutable,
            )
            .map_err(|e| anyhow::anyhow!("upsert_dependency_constraint: {:?}", e))?;
        Ok(())
    }

    /// Remove a dependency constraint.
    pub fn remove_dependency_constraint(
        &self,
        workspace: &str,
        constraint_kind: &str,
        source: &str,
        target: &str,
    ) -> Result<bool> {
        let ws = canonicalize_path(workspace);
        let params = params_map(&[
            ("ws", &ws),
            ("kind", constraint_kind),
            ("src", source),
            ("tgt", target),
        ]);
        let existing = self
            .run_script(
                "?[workspace, constraint_kind, source, target] := \
                    *dependency_constraint{workspace: $ws, constraint_kind: $kind, source: $src, target: $tgt} \
                 :rm dependency_constraint { workspace, constraint_kind, source, target }",
                params,
                ScriptMutability::Mutable,
            )
            .map_err(|e| anyhow::anyhow!("remove_dependency_constraint: {:?}", e))?;
        Ok(!existing.rows.is_empty())
    }

    /// List all layer assignments for a workspace.
    pub fn list_layer_assignments(&self, workspace: &str) -> Result<Vec<(String, String)>> {
        let ws = canonicalize_path(workspace);
        let params = params_map(&[("ws", &ws)]);
        let result = self
            .run_script(
                "?[context, layer] := *layer_assignment{workspace: $ws, context, layer}",
                params,
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("list_layer_assignments: {:?}", e))?;
        Ok(result
            .rows
            .iter()
            .map(|r| (dv_str(&r[0]), dv_str(&r[1])))
            .collect())
    }

    /// List all dependency constraints for a workspace.
    pub fn list_dependency_constraints(
        &self,
        workspace: &str,
    ) -> Result<Vec<(String, String, String, String)>> {
        let ws = canonicalize_path(workspace);
        let params = params_map(&[("ws", &ws)]);
        let result = self
            .run_script(
                "?[constraint_kind, source, target, rule] := \
                    *dependency_constraint{workspace: $ws, constraint_kind, source, target, rule}",
                params,
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("list_dependency_constraints: {:?}", e))?;
        Ok(result
            .rows
            .iter()
            .map(|r| (dv_str(&r[0]), dv_str(&r[1]), dv_str(&r[2]), dv_str(&r[3])))
            .collect())
    }

    /// Seed the built-in [`default_layer_constraints`] into the in-memory store.
    /// Idempotent and never persisted. Runtime overrides for the same key win.
    fn seed_default_constraints(&self, workspace: &str) -> Result<()> {
        let ws = canonicalize_path(workspace);
        for (kind, source, target, rule) in default_layer_constraints() {
            if !self.dependency_constraint_exists(&ws, kind, source, target)? {
                self.upsert_dependency_constraint_in_memory(&ws, kind, source, target, rule)?;
            }
        }
        Ok(())
    }

    fn dependency_constraint_exists(
        &self,
        workspace: &str,
        constraint_kind: &str,
        source: &str,
        target: &str,
    ) -> Result<bool> {
        let ws = canonicalize_path(workspace);
        let params = params_map(&[
            ("ws", &ws),
            ("kind", constraint_kind),
            ("src", source),
            ("tgt", target),
        ]);
        let result = self
            .run_script(
                "?[constraint_kind, source, target] := \
                    *dependency_constraint{workspace: $ws, constraint_kind, source, target}, \
                    constraint_kind = $kind, source = $src, target = $tgt",
                params,
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("dependency_constraint_exists: {:?}", e))?;
        Ok(!result.rows.is_empty())
    }

    /// Materialize convention-inferred layer assignments for the model's
    /// contexts. A context is assigned only when its name maps to a known layer
    /// (via [`crate::domain::analyze::classify_layer`]) *and* it has no existing
    /// assignment, so explicit runtime assignments are never overwritten.
    fn apply_inferred_layers(&self, workspace: &str, model: &DomainModel) -> Result<()> {
        let ws = canonicalize_path(workspace);
        let assigned = self
            .list_layer_assignments(&ws)?
            .into_iter()
            .map(|(context, _)| context)
            .collect::<BTreeSet<_>>();
        for bc in &model.bounded_contexts {
            if assigned.contains(&bc.name) {
                continue;
            }
            if let Some(layer) = crate::domain::analyze::classify_layer(&bc.name) {
                self.upsert_layer_assignment_in_memory(&ws, &bc.name, layer)?;
            }
        }
        Ok(())
    }

    /// Evaluate policy violations: find context dependencies that violate layer
    /// or context-level forbidden constraints.
    pub fn evaluate_policy_violations(&self, workspace: &str) -> Result<serde_json::Value> {
        let ws = canonicalize_path(workspace);
        self.refresh_runtime_constraints(&ws)?;
        self.evaluate_policy_violations_canonical(&ws)
    }

    fn evaluate_policy_violations_canonical(&self, ws: &str) -> Result<serde_json::Value> {
        let params = params_map(&[("ws", ws)]);

        // Layer-based violations: context A (layer X) depends on context B (layer Y)
        // where X→Y is forbidden
        let layer_violations = self
            .run_script(
                "allowed_layer[from_layer, to_layer] := \
                    *dependency_constraint{workspace: $ws, constraint_kind: 'layer', \
                        source: from_layer, target: to_layer, rule: 'allowed'} \
                 allowed_context[from_ctx, to_ctx] := \
                    *dependency_constraint{workspace: $ws, constraint_kind: 'context', \
                        source: from_ctx, target: to_ctx, rule: 'allowed'} \
                 ?[from_ctx, to_ctx, from_layer, to_layer] := \
                    *context_dep{workspace: $ws, from_ctx, to_ctx, state: 'actual' @ 'NOW'}, \
                    *layer_assignment{workspace: $ws, context: from_ctx, layer: from_layer}, \
                    *layer_assignment{workspace: $ws, context: to_ctx, layer: to_layer}, \
                    *dependency_constraint{workspace: $ws, constraint_kind: 'layer', \
                        source: from_layer, target: to_layer, rule: 'forbidden'}, \
                    not allowed_layer[from_layer, to_layer], \
                    not allowed_context[from_ctx, to_ctx]",
                params.clone(),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("policy layer violations: {:?}", e))?;

        // Context-level violations: context A depends on context B where A→B is forbidden
        let context_violations = self
            .run_script(
                "allowed_context[from_ctx, to_ctx] := \
                    *dependency_constraint{workspace: $ws, constraint_kind: 'context', \
                        source: from_ctx, target: to_ctx, rule: 'allowed'} \
                 ?[from_ctx, to_ctx] := \
                    *context_dep{workspace: $ws, from_ctx, to_ctx, state: 'actual' @ 'NOW'}, \
                    *dependency_constraint{workspace: $ws, constraint_kind: 'context', \
                        source: from_ctx, target: to_ctx, rule: 'forbidden'}, \
                    not allowed_context[from_ctx, to_ctx]",
                params.clone(),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("policy context violations: {:?}", e))?;

        let accepted_layer_deviations = self
            .run_script(
                "?[from_ctx, to_ctx, from_layer, to_layer] := \
                    *context_dep{workspace: $ws, from_ctx, to_ctx, state: 'actual' @ 'NOW'}, \
                    *layer_assignment{workspace: $ws, context: from_ctx, layer: from_layer}, \
                    *layer_assignment{workspace: $ws, context: to_ctx, layer: to_layer}, \
                    *dependency_constraint{workspace: $ws, constraint_kind: 'layer', \
                        source: from_layer, target: to_layer, rule: 'allowed'}",
                params.clone(),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("accepted layer deviations: {:?}", e))?;

        let accepted_context_deviations = self
            .run_script(
                "?[from_ctx, to_ctx] := \
                    *context_dep{workspace: $ws, from_ctx, to_ctx, state: 'actual' @ 'NOW'}, \
                    *dependency_constraint{workspace: $ws, constraint_kind: 'context', \
                        source: from_ctx, target: to_ctx, rule: 'allowed'}",
                params,
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("accepted context deviations: {:?}", e))?;

        let layer_items: Vec<serde_json::Value> = layer_violations
            .rows
            .iter()
            .map(|r| {
                json!({
                    "kind": "layer",
                    "from_context": dv_str(&r[0]),
                    "to_context": dv_str(&r[1]),
                    "from_layer": dv_str(&r[2]),
                    "to_layer": dv_str(&r[3]),
                    "rule": "forbidden",
                })
            })
            .collect();

        let context_items: Vec<serde_json::Value> = context_violations
            .rows
            .iter()
            .map(|r| {
                json!({
                    "kind": "context",
                    "from_context": dv_str(&r[0]),
                    "to_context": dv_str(&r[1]),
                    "rule": "forbidden",
                })
            })
            .collect();

        let accepted_layer_items: Vec<serde_json::Value> = accepted_layer_deviations
            .rows
            .iter()
            .map(|r| {
                json!({
                    "kind": "layer",
                    "from_context": dv_str(&r[0]),
                    "to_context": dv_str(&r[1]),
                    "from_layer": dv_str(&r[2]),
                    "to_layer": dv_str(&r[3]),
                    "rule": "allowed",
                })
            })
            .collect();

        let accepted_context_items: Vec<serde_json::Value> = accepted_context_deviations
            .rows
            .iter()
            .map(|r| {
                json!({
                    "kind": "context",
                    "from_context": dv_str(&r[0]),
                    "to_context": dv_str(&r[1]),
                    "rule": "allowed",
                })
            })
            .collect();

        let all_violations: Vec<serde_json::Value> =
            layer_items.into_iter().chain(context_items).collect();
        let accepted_deviations: Vec<serde_json::Value> = accepted_layer_items
            .into_iter()
            .chain(accepted_context_items)
            .collect();
        let warnings: Vec<serde_json::Value> = accepted_deviations
            .iter()
            .map(|deviation| {
                json!(format!(
                    "Accepted runtime architecture deviation: {} -> {} ({})",
                    deviation["from_context"].as_str().unwrap_or("?"),
                    deviation["to_context"].as_str().unwrap_or("?"),
                    deviation["kind"].as_str().unwrap_or("constraint")
                ))
            })
            .collect();
        let complexity = self.context_complexity(ws)?;
        let policy_coverage = self.policy_coverage(ws, &complexity)?;
        let configured = policy_coverage.context_count == 0
            || (policy_coverage.missing_layer_assignments.is_empty()
                && policy_coverage.dependency_constraint_count > 0);
        let status = if !all_violations.is_empty() {
            "false"
        } else if configured {
            "true"
        } else {
            "unconfigured"
        };

        Ok(json!({
            "status": status,
            "configured": configured,
            "policy_coverage": policy_coverage,
            "violations": all_violations,
            "count": all_violations.len(),
            "accepted_deviations": accepted_deviations,
            "accepted_deviation_count": accepted_deviations.len(),
            "warnings": warnings,
            "constraint_persistence": "runtime_only",
        }))
    }

    /// Return bounded, structured views over the persisted Rust graph.
    ///
    /// This is deliberately not an arbitrary Datalog endpoint. MCP clients get
    /// graph-database leverage through curated views that preserve relation
    /// names, node kinds, edge kinds, source evidence, and truncation metadata.
    pub fn query_rust_graph(&self, workspace: &str, args: &Value) -> Result<serde_json::Value> {
        let ws = canonicalize_path(workspace);
        let view = args["view"].as_str().unwrap_or("overview");
        let kind = args["kind"].as_str().unwrap_or("all");
        let relation = args["relation"].as_str().unwrap_or("all");
        let context_filter = args["context"]
            .as_str()
            .or_else(|| args["module"].as_str())
            .unwrap_or("");
        let file_filter = args["file"].as_str().unwrap_or("");
        let symbol_filter = args["symbol"]
            .as_str()
            .or_else(|| args["struct"].as_str())
            .unwrap_or("");
        let from_filter = args["from"].as_str().unwrap_or("");
        let to_filter = args["to"].as_str().unwrap_or("");
        let requested_scope =
            RustFactScope::parse(args["scope"].as_str().unwrap_or("all"), RustFactScope::All)?;
        let limit = u64_to_usize_saturating(args["limit"].as_u64().unwrap_or(50).clamp(1, 200));
        let offset = u64_to_usize_saturating(args["offset"].as_u64().unwrap_or(0).min(10_000));
        let filter_context = context_filter.to_lowercase();
        let filter_file = file_filter.to_lowercase();
        let filter_symbol = symbol_filter.to_lowercase();
        let filter_from = from_filter.to_lowercase();
        let filter_to = to_filter.to_lowercase();

        let filters = json!({
            "kind": kind,
            "relation": relation,
            "context": context_filter,
            "file": file_filter,
            "symbol": symbol_filter,
            "from": from_filter,
            "to": to_filter,
            "scope": requested_scope.as_str(),
            "limit": limit,
            "offset": offset,
        });

        let mut relations_used = BTreeSet::new();
        let truncate = |items: Vec<serde_json::Value>| {
            let total = items.len();
            let returned = items
                .into_iter()
                .skip(offset)
                .take(limit)
                .collect::<Vec<_>>();
            let next_offset = if offset + returned.len() < total {
                Some(offset + returned.len())
            } else {
                None
            };
            let truncated = next_offset.is_some();
            (returned, total, truncated, next_offset)
        };

        match view {
            "overview" | "relations" => {
                let mut relation_counts = self.rust_graph_relation_counts(&ws)?;
                if view == "relations" && relation != "all" {
                    relation_counts = relation_counts
                        .remove(relation)
                        .map(|count| BTreeMap::from([(relation.to_string(), count)]))
                        .unwrap_or_default();
                }
                for relation in relation_counts.keys() {
                    relations_used.insert(relation.clone());
                }
                Ok(json!({
                    "status": "ok",
                    "view": view,
                    "workspace": ws,
                    "graph_schema": rust_graph_schema_json(),
                    "relation_counts": relation_counts,
                    "relations_used": relations_used.into_iter().collect::<Vec<_>>(),
                    "filters": filters,
                    "summary": {
                        "relation_count": relation_counts.len(),
                    }
                }))
            }
            "nodes" => {
                let mut nodes = Vec::new();
                if (kind == "all" || kind == "context") && filter_symbol.is_empty() {
                    relations_used.insert("context".to_string());
                    let rows = self
                        .run_script(
                            "?[name, description, module_path] := *context{workspace: $ws, name, state: 'actual', description, module_path @ 'NOW'} :limit 500",
                            params_map(&[("ws", &ws)]),
                            ScriptMutability::Immutable,
                        )
                        .map_err(|e| anyhow::anyhow!("graph context nodes: {:?}", e))?;
                    for row in &rows.rows {
                        let name = dv_str(&row[0]);
                        let module_path = dv_str(&row[2]);
                        if !filter_context.is_empty()
                            && !text_matches(&name, &filter_context)
                            && !text_matches(&module_path, &filter_context)
                        {
                            continue;
                        }
                        nodes.push(json!({
                            "id": format!("context:{name}"),
                            "kind": "context",
                            "name": name,
                            "module_path": module_path,
                            "description": dv_str(&row[1]),
                            "relation": "context",
                        }));
                    }
                }
                if (kind == "all" || kind == "module") && filter_symbol.is_empty() {
                    relations_used.insert("module".to_string());
                    let rows = self
                        .run_script(
                            "?[context, name, path, public, file_path, description] := *module{workspace: $ws, context, name, state: 'actual', path, public, file_path, description @ 'NOW'} :limit 500",
                            params_map(&[("ws", &ws)]),
                            ScriptMutability::Immutable,
                        )
                        .map_err(|e| anyhow::anyhow!("graph module nodes: {:?}", e))?;
                    for row in &rows.rows {
                        let context = dv_str(&row[0]);
                        let name = dv_str(&row[1]);
                        let path = dv_str(&row[2]);
                        let file_path = dv_str(&row[4]);
                        if !rust_fact_allowed(requested_scope, &file_path, &name, "") {
                            continue;
                        }
                        if !filter_context.is_empty()
                            && !text_matches(&context, &filter_context)
                            && !text_matches(&name, &filter_context)
                            && !text_matches(&path, &filter_context)
                        {
                            continue;
                        }
                        if !filter_file.is_empty() && !text_matches(&file_path, &filter_file) {
                            continue;
                        }
                        nodes.push(json!({
                            "id": format!("module:{path}"),
                            "kind": "module",
                            "name": name,
                            "context": context,
                            "path": path,
                            "public": matches!(&row[3], cozo::DataValue::Bool(true)),
                            "file": file_path,
                            "description": dv_str(&row[5]),
                            "relation": "module",
                        }));
                    }
                }
                if (kind == "all" || kind == "source_file") && filter_symbol.is_empty() {
                    relations_used.insert("source_file".to_string());
                    let rows = self
                        .run_script(
                            "?[path, context, language] := *source_file{workspace: $ws, path, state: 'actual', context, language @ 'NOW'} :limit 500",
                            params_map(&[("ws", &ws)]),
                            ScriptMutability::Immutable,
                        )
                        .map_err(|e| anyhow::anyhow!("graph source_file nodes: {:?}", e))?;
                    for row in &rows.rows {
                        let path = dv_str(&row[0]);
                        let context = dv_str(&row[1]);
                        if !rust_fact_allowed(requested_scope, &path, "", "") {
                            continue;
                        }
                        if !filter_context.is_empty() && !text_matches(&context, &filter_context) {
                            continue;
                        }
                        if !filter_file.is_empty() && !text_matches(&path, &filter_file) {
                            continue;
                        }
                        nodes.push(json!({
                            "id": format!("source_file:{path}"),
                            "kind": "source_file",
                            "path": path,
                            "context": context,
                            "language": dv_str(&row[2]),
                            "relation": "source_file",
                        }));
                    }
                }
                if kind == "all"
                    || kind == "symbol"
                    || matches!(kind, "struct" | "enum" | "function" | "method")
                {
                    relations_used.insert("symbol".to_string());
                    let rows = self
                        .run_script(
                            "?[name, kind, context, file_path, start_line, end_line, visibility] := *symbol{workspace: $ws, name, state: 'actual', kind, context, file_path, start_line, end_line, visibility @ 'NOW'} :limit 500",
                            params_map(&[("ws", &ws)]),
                            ScriptMutability::Immutable,
                        )
                        .map_err(|e| anyhow::anyhow!("graph symbol nodes: {:?}", e))?;
                    for row in &rows.rows {
                        let name = dv_str(&row[0]);
                        let symbol_kind = dv_str(&row[1]);
                        let context = dv_str(&row[2]);
                        let file_path = dv_str(&row[3]);
                        if !rust_fact_allowed(requested_scope, &file_path, &name, "") {
                            continue;
                        }
                        if kind != "all" && kind != "symbol" && kind != symbol_kind {
                            continue;
                        }
                        if !filter_symbol.is_empty() && !text_matches(&name, &filter_symbol) {
                            continue;
                        }
                        if !filter_context.is_empty() && !text_matches(&context, &filter_context) {
                            continue;
                        }
                        if !filter_file.is_empty() && !text_matches(&file_path, &filter_file) {
                            continue;
                        }
                        nodes.push(json!({
                            "id": format!("symbol:{name}"),
                            "kind": symbol_kind,
                            "name": name,
                            "context": context,
                            "file": file_path,
                            "start_line": dv_i64(&row[4]),
                            "end_line": dv_i64(&row[5]),
                            "visibility": dv_str(&row[6]),
                            "relation": "symbol",
                        }));
                    }
                }
                let (nodes, total, truncated, next_offset) = truncate(nodes);
                Ok(json!({
                    "status": "ok",
                    "view": "nodes",
                    "workspace": ws,
                    "nodes": nodes,
                    "count": nodes.len(),
                    "total_before_limit": total,
                    "truncated": truncated,
                    "next_offset": next_offset,
                    "exhaustiveness": {
                        "exhaustive": offset == 0 && !truncated,
                        "page_complete": !truncated,
                        "offset": offset,
                        "limit": limit,
                        "returned": nodes.len(),
                        "total_before_limit": total,
                        "next_offset": next_offset,
                    },
                    "relations_used": relations_used.into_iter().collect::<Vec<_>>(),
                    "filters": filters,
                }))
            }
            "edges" => {
                let mut edges = Vec::new();
                if relation == "all" || relation == "context_dep" {
                    relations_used.insert("context_dep".to_string());
                    // Drop the in-DB row cap when a filter is active so the Rust-side
                    // filter sees every row (the final result is capped by `truncate`).
                    let context_dep_query = if filter_context.is_empty()
                        && filter_from.is_empty()
                        && filter_to.is_empty()
                    {
                        "?[from_ctx, to_ctx] := *context_dep{workspace: $ws, from_ctx, to_ctx, state: 'actual' @ 'NOW'} :limit 500"
                    } else {
                        "?[from_ctx, to_ctx] := *context_dep{workspace: $ws, from_ctx, to_ctx, state: 'actual' @ 'NOW'}"
                    };
                    let rows = self
                        .run_script(
                            context_dep_query,
                            params_map(&[("ws", &ws)]),
                            ScriptMutability::Immutable,
                        )
                        .map_err(|e| anyhow::anyhow!("graph context_dep edges: {:?}", e))?;
                    for row in &rows.rows {
                        let from = dv_str(&row[0]);
                        let to = dv_str(&row[1]);
                        if same_context_name(&from, &to) {
                            continue;
                        }
                        if !filter_context.is_empty()
                            && !text_matches(&from, &filter_context)
                            && !text_matches(&to, &filter_context)
                        {
                            continue;
                        }
                        if !filter_from.is_empty() && !text_matches(&from, &filter_from) {
                            continue;
                        }
                        if !filter_to.is_empty() && !text_matches(&to, &filter_to) {
                            continue;
                        }
                        edges.push(json!({
                            "id": format!("context_dep:{from}->{to}"),
                            "relation": "context_dep",
                            "from": from,
                            "to": to,
                            "from_kind": "context",
                            "to_kind": "context",
                        }));
                    }
                }
                if relation == "all" || relation == "import_edge" {
                    relations_used.insert("import_edge".to_string());
                    let import_edge_query = if filter_context.is_empty()
                        && filter_file.is_empty()
                        && filter_symbol.is_empty()
                        && filter_from.is_empty()
                        && filter_to.is_empty()
                    {
                        "?[from_file, to_module, context] := *import_edge{workspace: $ws, from_file, to_module, state: 'actual', context @ 'NOW'} :limit 500"
                    } else {
                        "?[from_file, to_module, context] := *import_edge{workspace: $ws, from_file, to_module, state: 'actual', context @ 'NOW'}"
                    };
                    let rows = self
                        .run_script(
                            import_edge_query,
                            params_map(&[("ws", &ws)]),
                            ScriptMutability::Immutable,
                        )
                        .map_err(|e| anyhow::anyhow!("graph import_edge edges: {:?}", e))?;
                    for row in &rows.rows {
                        let from_file = dv_str(&row[0]);
                        let to_module = dv_str(&row[1]);
                        let context = dv_str(&row[2]);
                        if !rust_fact_allowed(requested_scope, &from_file, "", &to_module) {
                            continue;
                        }
                        if !filter_context.is_empty() && !text_matches(&context, &filter_context) {
                            continue;
                        }
                        if !filter_file.is_empty() && !text_matches(&from_file, &filter_file) {
                            continue;
                        }
                        if !filter_symbol.is_empty() && !text_matches(&to_module, &filter_symbol) {
                            continue;
                        }
                        if !filter_from.is_empty() && !text_matches(&from_file, &filter_from) {
                            continue;
                        }
                        if !filter_to.is_empty() && !text_matches(&to_module, &filter_to) {
                            continue;
                        }
                        edges.push(json!({
                            "id": format!("import_edge:{from_file}->{to_module}"),
                            "relation": "import_edge",
                            "from": from_file,
                            "to": to_module,
                            "from_kind": "source_file",
                            "to_kind": "module_path",
                            "context": context,
                        }));
                    }
                }
                if relation == "all" || relation == "reference_edge" {
                    relations_used.insert("reference_edge".to_string());
                    let reference_edge_query = if filter_context.is_empty()
                        && filter_file.is_empty()
                        && filter_symbol.is_empty()
                        && filter_from.is_empty()
                        && filter_to.is_empty()
                    {
                        "?[from_file, to_path, reference_kind, line, context] := *reference_edge{workspace: $ws, from_file, to_path, reference_kind, line, state: 'actual', context @ 'NOW'} :limit 500"
                    } else {
                        "?[from_file, to_path, reference_kind, line, context] := *reference_edge{workspace: $ws, from_file, to_path, reference_kind, line, state: 'actual', context @ 'NOW'}"
                    };
                    let rows = self
                        .run_script(
                            reference_edge_query,
                            params_map(&[("ws", &ws)]),
                            ScriptMutability::Immutable,
                        )
                        .map_err(|e| anyhow::anyhow!("graph reference_edge edges: {:?}", e))?;
                    for row in &rows.rows {
                        let from_file = dv_str(&row[0]);
                        let to_path = dv_str(&row[1]);
                        let reference_kind = dv_str(&row[2]);
                        let context = dv_str(&row[4]);
                        if !rust_fact_allowed(
                            requested_scope,
                            &from_file,
                            &to_path,
                            &reference_kind,
                        ) {
                            continue;
                        }
                        if !filter_context.is_empty() && !text_matches(&context, &filter_context) {
                            continue;
                        }
                        if !filter_file.is_empty() && !text_matches(&from_file, &filter_file) {
                            continue;
                        }
                        if !filter_symbol.is_empty()
                            && !text_matches(&to_path, &filter_symbol)
                            && !text_matches(&reference_kind, &filter_symbol)
                        {
                            continue;
                        }
                        if !filter_from.is_empty() && !text_matches(&from_file, &filter_from) {
                            continue;
                        }
                        if !filter_to.is_empty() && !text_matches(&to_path, &filter_to) {
                            continue;
                        }
                        edges.push(json!({
                            "id": format!("reference_edge:{from_file}->{to_path}:{}", dv_i64(&row[3])),
                            "relation": "reference_edge",
                            "from": from_file,
                            "to": to_path,
                            "from_kind": "source_file",
                            "to_kind": "rust_path",
                            "reference_kind": reference_kind,
                            "file": dv_str(&row[0]),
                            "line": dv_i64(&row[3]),
                            "context": context,
                        }));
                    }
                }
                if relation == "all" || relation == "calls_symbol" {
                    relations_used.insert("calls_symbol".to_string());
                    let calls_query = if filter_context.is_empty()
                        && filter_file.is_empty()
                        && filter_symbol.is_empty()
                        && filter_from.is_empty()
                        && filter_to.is_empty()
                    {
                        "?[caller, callee, file_path, line, context] := *calls_symbol{workspace: $ws, caller, callee, state: 'actual', file_path, line, context @ 'NOW'} :limit 500"
                    } else {
                        "?[caller, callee, file_path, line, context] := *calls_symbol{workspace: $ws, caller, callee, state: 'actual', file_path, line, context @ 'NOW'}"
                    };
                    let rows = self
                        .run_script(
                            calls_query,
                            params_map(&[("ws", &ws)]),
                            ScriptMutability::Immutable,
                        )
                        .map_err(|e| anyhow::anyhow!("graph calls_symbol edges: {:?}", e))?;
                    for row in &rows.rows {
                        let caller = dv_str(&row[0]);
                        let callee = dv_str(&row[1]);
                        let file_path = dv_str(&row[2]);
                        let context = dv_str(&row[4]);
                        if !rust_fact_allowed(requested_scope, &file_path, &caller, &callee) {
                            continue;
                        }
                        if !filter_context.is_empty() && !text_matches(&context, &filter_context) {
                            continue;
                        }
                        if !filter_file.is_empty() && !text_matches(&file_path, &filter_file) {
                            continue;
                        }
                        if !filter_symbol.is_empty()
                            && !text_matches(&caller, &filter_symbol)
                            && !text_matches(&callee, &filter_symbol)
                        {
                            continue;
                        }
                        if !filter_from.is_empty() && !text_matches(&caller, &filter_from) {
                            continue;
                        }
                        if !filter_to.is_empty() && !text_matches(&callee, &filter_to) {
                            continue;
                        }
                        edges.push(json!({
                            "id": format!("calls_symbol:{caller}->{callee}:{}", dv_i64(&row[3])),
                            "relation": "calls_symbol",
                            "from": caller,
                            "to": callee,
                            "from_kind": "symbol",
                            "to_kind": "symbol",
                            "file": file_path,
                            "line": dv_i64(&row[3]),
                            "context": context,
                        }));
                    }
                }
                if relation == "all" || relation == "ast_edge" {
                    relations_used.insert("ast_edge".to_string());
                    let ast_edge_query = if filter_symbol.is_empty()
                        && filter_from.is_empty()
                        && filter_to.is_empty()
                    {
                        "?[from_node, to_node, edge_type, file_path, line] := *ast_edge{workspace: $ws, state: 'actual', from_node, to_node, edge_type, file_path, line @ 'NOW'} :limit 500"
                    } else {
                        "?[from_node, to_node, edge_type, file_path, line] := *ast_edge{workspace: $ws, state: 'actual', from_node, to_node, edge_type, file_path, line @ 'NOW'}"
                    };
                    let rows = self
                        .run_script(
                            ast_edge_query,
                            params_map(&[("ws", &ws)]),
                            ScriptMutability::Immutable,
                        )
                        .map_err(|e| anyhow::anyhow!("graph ast_edge edges: {:?}", e))?;
                    for row in &rows.rows {
                        let from_node = dv_str(&row[0]);
                        let to_node = dv_str(&row[1]);
                        let file_path = dv_str(&row[3]);
                        if !rust_fact_allowed(requested_scope, &file_path, &from_node, &to_node) {
                            continue;
                        }
                        if !filter_symbol.is_empty()
                            && !text_matches(&from_node, &filter_symbol)
                            && !text_matches(&to_node, &filter_symbol)
                        {
                            continue;
                        }
                        if !filter_from.is_empty() && !text_matches(&from_node, &filter_from) {
                            continue;
                        }
                        if !filter_to.is_empty() && !text_matches(&to_node, &filter_to) {
                            continue;
                        }
                        edges.push(json!({
                            "id": format!("ast_edge:{from_node}->{to_node}"),
                            "relation": "ast_edge",
                            "from": from_node,
                            "to": to_node,
                            "from_kind": "symbol",
                            "to_kind": "symbol",
                            "edge_type": dv_str(&row[2]),
                            "file": file_path,
                            "line": dv_i64(&row[4]),
                        }));
                    }
                }
                if relation == "all" || relation == "resolved_call" {
                    relations_used.insert("resolved_call".to_string());
                    let resolved_call_query = if filter_symbol.is_empty()
                        && filter_from.is_empty()
                        && filter_to.is_empty()
                    {
                        "?[caller, callee, callee_file, callee_line, caller_file, caller_line, call_site_line, call_expr, dispatch_kind] := *resolved_call{workspace: $ws, state: 'actual', caller, callee, callee_file, callee_line, caller_file, caller_line, call_site_line, call_expr, dispatch_kind @ 'NOW'} :limit 500"
                    } else {
                        "?[caller, callee, callee_file, callee_line, caller_file, caller_line, call_site_line, call_expr, dispatch_kind] := *resolved_call{workspace: $ws, state: 'actual', caller, callee, callee_file, callee_line, caller_file, caller_line, call_site_line, call_expr, dispatch_kind @ 'NOW'}"
                    };
                    let rows = self
                        .run_script(
                            resolved_call_query,
                            params_map(&[("ws", &ws)]),
                            ScriptMutability::Immutable,
                        )
                        .map_err(|e| anyhow::anyhow!("graph resolved_call edges: {:?}", e))?;
                    for row in &rows.rows {
                        let caller = dv_str(&row[0]);
                        let callee = dv_str(&row[1]);
                        let callee_file = dv_str(&row[2]);
                        if !rust_fact_allowed(requested_scope, &callee_file, &caller, &callee) {
                            continue;
                        }
                        // `symbol`/`from` match the caller; `to` matches the resolved
                        // callee. So `from="Store::save"` lists what it actually calls.
                        if !filter_symbol.is_empty()
                            && !text_matches(&caller, &filter_symbol)
                            && !text_matches(&callee, &filter_symbol)
                        {
                            continue;
                        }
                        if !filter_from.is_empty() && !text_matches(&caller, &filter_from) {
                            continue;
                        }
                        if !filter_to.is_empty() && !text_matches(&callee, &filter_to) {
                            continue;
                        }
                        edges.push(json!({
                            "id": format!("resolved_call:{caller}->{callee}"),
                            "relation": "resolved_call",
                            "from": caller,
                            "to": callee,
                            "from_kind": "symbol",
                            "to_kind": "symbol",
                            "edge_type": "calls",
                            "file": callee_file,
                            "line": dv_i64(&row[3]),
                            "caller_file": dv_str(&row[4]),
                            "caller_line": dv_i64(&row[5]),
                            "call_site_line": dv_i64(&row[6]),
                            "call_expr": dv_str(&row[7]),
                            "dispatch_kind": dv_str(&row[8]),
                        }));
                    }
                }
                let (edges, total, truncated, next_offset) = truncate(edges);
                Ok(json!({
                    "status": "ok",
                    "view": "edges",
                    "workspace": ws,
                    "edges": edges,
                    "count": edges.len(),
                    "total_before_limit": total,
                    "truncated": truncated,
                    "next_offset": next_offset,
                    "exhaustiveness": {
                        "exhaustive": offset == 0 && !truncated,
                        "page_complete": !truncated,
                        "offset": offset,
                        "limit": limit,
                        "returned": edges.len(),
                        "total_before_limit": total,
                        "next_offset": next_offset,
                    },
                    "relations_used": relations_used.into_iter().collect::<Vec<_>>(),
                    "filters": filters,
                }))
            }
            "neighborhood" => {
                if context_filter.is_empty() && file_filter.is_empty() && symbol_filter.is_empty() {
                    anyhow::bail!(
                        "neighborhood requires one of 'context'/'module', 'file', or 'symbol'"
                    );
                }
                let nodes = self.query_rust_graph(
                    &ws,
                    &json!({
                        "view": "nodes",
                        "kind": kind,
                        "context": context_filter,
                        "file": file_filter,
                        "symbol": symbol_filter,
                        "scope": requested_scope.as_str(),
                        "limit": limit,
                        "offset": offset,
                    }),
                )?;
                let edges = self.query_rust_graph(
                    &ws,
                    &json!({
                        "view": "edges",
                        "relation": relation,
                        "context": context_filter,
                        "file": file_filter,
                        "symbol": symbol_filter,
                        "scope": requested_scope.as_str(),
                        "limit": limit,
                        "offset": offset,
                    }),
                )?;
                let mut neighborhood_nodes = nodes["nodes"].as_array().cloned().unwrap_or_default();
                let mut node_ids = neighborhood_nodes
                    .iter()
                    .filter_map(|node| node["id"].as_str().map(str::to_string))
                    .collect::<BTreeSet<_>>();
                if let Some(edge_values) = edges["edges"].as_array() {
                    for edge in edge_values {
                        for (name_field, kind_field) in [("from", "from_kind"), ("to", "to_kind")] {
                            let name = edge[name_field].as_str().unwrap_or("");
                            if name.is_empty() {
                                continue;
                            }
                            let node_kind = edge[kind_field].as_str().unwrap_or("node");
                            let node_id = format!("{node_kind}:{name}");
                            if node_ids.insert(node_id.clone()) {
                                neighborhood_nodes.push(json!({
                                    "id": node_id,
                                    "kind": node_kind,
                                    "name": name,
                                    "context": edge["context"].as_str().unwrap_or(""),
                                    "file": edge["file"].as_str().unwrap_or(""),
                                    "relation": edge["relation"].as_str().unwrap_or("edge_endpoint"),
                                }));
                            }
                        }
                    }
                }
                let mut relation_values = BTreeSet::new();
                for relation in nodes["relations_used"].as_array().into_iter().flatten() {
                    if let Some(relation) = relation.as_str() {
                        relation_values.insert(relation.to_string());
                    }
                }
                for relation in edges["relations_used"].as_array().into_iter().flatten() {
                    if let Some(relation) = relation.as_str() {
                        relation_values.insert(relation.to_string());
                    }
                }
                let node_count = neighborhood_nodes.len() as u64;
                let edge_count = edges["count"].as_u64().unwrap_or(0);
                let truncated = nodes["truncated"].as_bool().unwrap_or(false)
                    || edges["truncated"].as_bool().unwrap_or(false);
                Ok(json!({
                    "status": "ok",
                    "view": "neighborhood",
                    "workspace": ws,
                    "focal": {
                        "context": context_filter,
                        "file": file_filter,
                        "symbol": symbol_filter,
                    },
                    "nodes": neighborhood_nodes,
                    "edges": edges["edges"].clone(),
                    "count": node_count + edge_count,
                    "summary": {
                        "node_count": node_count,
                        "edge_count": edge_count,
                    },
                    "truncated": truncated,
                    "next_offset": {
                        "nodes": nodes["next_offset"],
                        "edges": edges["next_offset"],
                    },
                    "exhaustiveness": {
                        "exhaustive": offset == 0 && !truncated,
                        "page_complete": !truncated,
                        "offset": offset,
                        "limit": limit,
                        "returned": node_count + edge_count,
                        "nodes": nodes["exhaustiveness"],
                        "edges": edges["exhaustiveness"],
                    },
                    "relations_used": relation_values.into_iter().collect::<Vec<_>>(),
                    "filters": filters,
                }))
            }
            "paths" => {
                if relation == "all" || relation == "context_dep" {
                    if from_filter.is_empty() || to_filter.is_empty() {
                        anyhow::bail!("paths with context_dep require 'from' and 'to'");
                    }
                    relations_used.insert("context_dep".to_string());
                    let paths = self.query_dependency_path(&ws, from_filter, to_filter)?;
                    Ok(json!({
                        "status": "ok",
                        "view": "paths",
                        "workspace": ws,
                        "relation": "context_dep",
                        "from": from_filter,
                        "to": to_filter,
                        "reachable": !paths.is_empty(),
                        "paths": paths,
                        "count": paths.len(),
                        "relations_used": relations_used.into_iter().collect::<Vec<_>>(),
                        "filters": filters,
                    }))
                } else if relation == "calls_symbol" {
                    if from_filter.is_empty() {
                        anyhow::bail!("paths with calls_symbol require 'from'");
                    }
                    relations_used.insert("calls_symbol".to_string());
                    if to_filter.is_empty() {
                        let result = self.call_graph_reachability(&ws, from_filter)?;
                        Ok(json!({
                            "status": "ok",
                            "view": "paths",
                            "workspace": ws,
                            "relation": "calls_symbol",
                            "from": from_filter,
                            "to": to_filter,
                            "reachable": result["count"].as_u64().unwrap_or(0) > 0,
                            "result": result,
                            "count": result["count"],
                            "relations_used": relations_used.into_iter().collect::<Vec<_>>(),
                            "filters": filters,
                        }))
                    } else {
                        let paths = self.query_call_paths(&ws, from_filter, to_filter)?;
                        Ok(json!({
                            "status": "ok",
                            "view": "paths",
                            "workspace": ws,
                            "relation": "calls_symbol",
                            "from": from_filter,
                            "to": to_filter,
                            "reachable": !paths.is_empty(),
                            "paths": paths,
                            "count": paths.len(),
                            "relations_used": relations_used.into_iter().collect::<Vec<_>>(),
                            "filters": filters,
                        }))
                    }
                } else {
                    anyhow::bail!("paths supports relation 'context_dep' or 'calls_symbol'");
                }
            }
            other => anyhow::bail!(
                "Unknown graph view '{other}'. Use overview, relations, nodes, edges, neighborhood, or paths."
            ),
        }
    }

    pub fn rust_graph_relation_counts(&self, workspace: &str) -> Result<BTreeMap<String, usize>> {
        let mut counts = BTreeMap::new();
        let relations = [
            (
                "context",
                "?[count(name)] := *context{workspace: $ws, name, state: 'actual' @ 'NOW'}",
            ),
            (
                "module",
                "?[count(name)] := *module{workspace: $ws, name, state: 'actual' @ 'NOW'}",
            ),
            (
                "source_file",
                "?[count(path)] := *source_file{workspace: $ws, path, state: 'actual' @ 'NOW'}",
            ),
            (
                "symbol",
                "?[count(name)] := *symbol{workspace: $ws, name, state: 'actual' @ 'NOW'}",
            ),
            (
                "context_dep",
                "?[count(from_ctx)] := *context_dep{workspace: $ws, from_ctx, to_ctx, state: 'actual' @ 'NOW'}, from_ctx != to_ctx",
            ),
            (
                "import_edge",
                "?[count(from_file)] := *import_edge{workspace: $ws, from_file, state: 'actual' @ 'NOW'}",
            ),
            (
                "reference_edge",
                "?[count(from_file)] := *reference_edge{workspace: $ws, from_file, state: 'actual' @ 'NOW'}",
            ),
            (
                "calls_symbol",
                "?[count(caller)] := *calls_symbol{workspace: $ws, caller, state: 'actual' @ 'NOW'}",
            ),
            (
                "resolved_call",
                "?[count(caller)] := *resolved_call{workspace: $ws, caller, state: 'actual' @ 'NOW'}",
            ),
            (
                "ast_edge",
                "?[count(from_node)] := *ast_edge{workspace: $ws, from_node, state: 'actual' @ 'NOW'}",
            ),
        ];

        for (relation, query) in relations {
            let rows = self
                .run_script(
                    query,
                    params_map(&[("ws", workspace)]),
                    ScriptMutability::Immutable,
                )
                .map_err(|e| anyhow::anyhow!("graph relation count {relation}: {:?}", e))?;
            let count = rows.rows.first().map(|row| dv_i64(&row[0])).unwrap_or(0);
            counts.insert(relation.to_string(), i64_to_usize_saturating(count.max(0)));
        }

        Ok(counts)
    }

    pub fn impact_analysis(
        &self,
        workspace: &str,
        context: &str,
        entity_name: &str,
    ) -> Result<serde_json::Value> {
        let params = params_map(&[("ws", workspace), ("ctx", context), ("ent", entity_name)]);

        let events = self
            .run_script(
                "?[context, event_name] := \
                    *event{workspace: $ws, context, name: event_name, source: $ent, state: 'actual' @ 'NOW'}",
                params.clone(),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("impact events: {:?}", e))?;

        let services = self
            .run_script(
                "?[context, service_name] := \
                    *repository{workspace: $ws, context: $ctx, aggregate: $ent, name: repo_name, state: 'actual' @ 'NOW'}, \
                    *service_dep{workspace: $ws, context, service: service_name, dep: repo_name, state: 'actual' @ 'NOW'}",
                params.clone(),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("impact services: {:?}", e))?;

        let reverse_params = params_map(&[("ws", workspace), ("ctx", context)]);
        let dependents = self
            .run_script(
                "transitive[a, c] := *context_dep{workspace: $ws, from_ctx: a, to_ctx: c, state: 'actual' @ 'NOW'} \
                 transitive[a, c] := transitive[a, b], *context_dep{workspace: $ws, from_ctx: b, to_ctx: c, state: 'actual' @ 'NOW'} \
                 ?[dependent] := transitive[dependent, $ctx]",
                reverse_params,
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("impact dependents: {:?}", e))?;

        let ast_impact = self
            .run_script(
                "ast[target, type] := *ast_edge{workspace: $ws, from_node: $ent, to_node: target, edge_type: type @ 'NOW'} \
                 ast[target, type] := ast[mid, _], *ast_edge{workspace: $ws, from_node: mid, to_node: target, edge_type: type @ 'NOW'} \
                 ?[target, type] := ast[target, type]",
                params_map(&[("ws", workspace), ("ent", entity_name)]),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("ast impact: {:?}", e))?;

        // Symbol-level: find files that import modules containing this entity name
        let importing_files = self
            .run_script(
                "?[from_file, to_module, context] := *import_edge{workspace: $ws, from_file, to_module, context @ 'NOW'}",
                params_map(&[("ws", workspace)]),
                ScriptMutability::Immutable,
            )
            .map(|r| r.rows)
            .unwrap_or_default()
            .into_iter()
            .filter(|row| import_references_symbol(&dv_str(&row[1]), entity_name))
            .collect::<Vec<_>>();

        Ok(json!({
            "entity": entity_name,
            "context": context,
            "affected_events": events.rows.iter()
                .map(|r| json!({"context": dv_str(&r[0]), "event": dv_str(&r[1])}))
                .collect::<Vec<_>>(),
            "affected_services": services.rows.iter()
                .map(|r| json!({"context": dv_str(&r[0]), "service": dv_str(&r[1])}))
                .collect::<Vec<_>>(),
            "dependent_contexts": dependents.rows.iter()
                .map(|r| dv_str(&r[0]))
                .collect::<Vec<_>>(),
            "ast_impact": ast_impact.rows.iter()
                .map(|r| json!({"target": dv_str(&r[0]), "type": dv_str(&r[1])}))
                .collect::<Vec<_>>(),
            "importing_files": importing_files.iter()
                .map(|r| json!({"file": dv_str(&r[0]), "import": dv_str(&r[1]), "context": dv_str(&r[2])}))
                .collect::<Vec<_>>(),
        }))
    }

    pub fn aggregate_roots_without_invariants(
        &self,
        workspace: &str,
    ) -> Result<Vec<(String, String)>> {
        let params = params_map(&[("ws", workspace)]);
        let result = self
            .run_script(
                "has_inv[ctx, ent] := *invariant{workspace: $ws, context: ctx, entity: ent, state: 'actual' @ 'NOW'} \
                 ?[context, entity] := \
                    *entity{workspace: $ws, context, name: entity, aggregate_root: true, state: 'actual' @ 'NOW'}, \
                    not has_inv[context, entity]",
                params,
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("aggregate_roots_without_invariants: {:?}", e))?;
        Ok(result
            .rows
            .iter()
            .map(|r| (dv_str(&r[0]), dv_str(&r[1])))
            .collect())
    }

    pub fn query_dependency_path(
        &self,
        workspace: &str,
        from_context: &str,
        to_context: &str,
    ) -> Result<Vec<Vec<String>>> {
        let params = params_map(&[("ws", workspace)]);
        let result = self
            .run_script(
                "?[from_ctx, to_ctx] := *context_dep{workspace: $ws, from_ctx, to_ctx @ 'NOW'}",
                params,
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("query_dependency_path: {:?}", e))?;

        if from_context == to_context {
            return Ok(vec![vec![from_context.to_string()]]);
        }

        let mut adjacency: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut contexts = BTreeSet::new();
        for row in &result.rows {
            let from_ctx = dv_str(&row[0]);
            let to_ctx = dv_str(&row[1]);
            contexts.insert(from_ctx.clone());
            contexts.insert(to_ctx.clone());
            adjacency.entry(from_ctx).or_default().push(to_ctx);
        }
        for targets in adjacency.values_mut() {
            targets.sort();
            targets.dedup();
        }

        let max_depth = contexts.len().max(1);
        let mut paths = Vec::new();
        let mut path = vec![from_context.to_string()];
        let mut visited = BTreeSet::from([from_context.to_string()]);
        collect_dependency_paths(
            from_context,
            to_context,
            &adjacency,
            &mut visited,
            &mut path,
            &mut paths,
            max_depth,
        );
        Ok(paths)
    }

    pub fn query_call_paths(
        &self,
        workspace: &str,
        from_symbol: &str,
        to_symbol: &str,
    ) -> Result<Vec<Vec<String>>> {
        let ws = canonicalize_path(workspace);
        let params = params_map(&[("ws", &ws)]);
        let result = self
            .run_script(
                "?[caller, callee] := *calls_symbol{workspace: $ws, caller, callee, state: 'actual' @ 'NOW'}",
                params,
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("query_call_paths: {:?}", e))?;

        if from_symbol == to_symbol {
            return Ok(vec![vec![from_symbol.to_string()]]);
        }

        let mut adjacency: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut symbols = BTreeSet::new();
        for row in &result.rows {
            let caller = dv_str(&row[0]);
            let callee = dv_str(&row[1]);
            symbols.insert(caller.clone());
            symbols.insert(callee.clone());
            adjacency.entry(caller).or_default().push(callee);
        }
        for targets in adjacency.values_mut() {
            targets.sort();
            targets.dedup();
        }

        let start_symbols = symbols
            .iter()
            .filter(|symbol| symbol_lookup_matches(symbol, from_symbol))
            .cloned()
            .collect::<Vec<_>>();
        let max_depth = symbols.len().max(1);
        let mut paths = Vec::new();
        for start_symbol in start_symbols {
            let mut path = vec![start_symbol.clone()];
            let mut visited = BTreeSet::from([start_symbol.clone()]);
            collect_dependency_paths(
                &start_symbol,
                to_symbol,
                &adjacency,
                &mut visited,
                &mut path,
                &mut paths,
                max_depth,
            );
        }
        Ok(paths)
    }

    pub fn can_delete_symbol(
        &self,
        workspace: &str,
        context: &str,
        entity_name: &str,
    ) -> Result<serde_json::Value> {
        let params = params_map(&[("ws", workspace), ("ctx", context), ("ent", entity_name)]);
        let workspace_params = params_map(&[("ws", workspace), ("ent", entity_name)]);

        let aggreg = if context.is_empty() {
            self.run_script(
                "?[context, agg] := *aggregate_member{workspace: $ws, context, member: $ent, state: 'actual', aggregate: agg @ 'NOW'}",
                workspace_params.clone(),
                ScriptMutability::Immutable,
            )
        } else {
            self.run_script(
                "?[context, agg] := *aggregate_member{workspace: $ws, context, member: $ent, state: 'actual', aggregate: agg @ 'NOW'}, context = $ctx",
                params.clone(),
                ScriptMutability::Immutable,
            )
        }
        .map_err(|e| anyhow::anyhow!("check aggregate: {:?}", e))?;

        let events = if context.is_empty() {
            self.run_script(
                "?[context, evt, file_path, start_line, end_line] := *event{workspace: $ws, context, source: $ent, state: 'actual', name: evt, file_path, start_line, end_line @ 'NOW'}",
                workspace_params.clone(),
                ScriptMutability::Immutable,
            )
        } else {
            self.run_script(
                "?[context, evt, file_path, start_line, end_line] := *event{workspace: $ws, context, source: $ent, state: 'actual', name: evt, file_path, start_line, end_line @ 'NOW'}, context = $ctx",
                params.clone(),
                ScriptMutability::Immutable,
            )
        }
        .map_err(|e| anyhow::anyhow!("check events: {:?}", e))?;

        let repos = if context.is_empty() {
            self.run_script(
                "?[context, repo, file_path, start_line, end_line] := *repository{workspace: $ws, context, aggregate: $ent, state: 'actual', name: repo, file_path, start_line, end_line @ 'NOW'}",
                workspace_params.clone(),
                ScriptMutability::Immutable,
            )
        } else {
            self.run_script(
                "?[context, repo, file_path, start_line, end_line] := *repository{workspace: $ws, context, aggregate: $ent, state: 'actual', name: repo, file_path, start_line, end_line @ 'NOW'}, context = $ctx",
                params.clone(),
                ScriptMutability::Immutable,
            )
        }
        .map_err(|e| anyhow::anyhow!("check repo: {:?}", e))?;

        let has_deps = !aggreg.rows.is_empty() || !events.rows.is_empty() || !repos.rows.is_empty();

        // Symbol-level: check if any import edges reference this symbol
        let import_refs = self.run_script(
            "?[from_file, to_module, context] := *import_edge{workspace: $ws, from_file, to_module, context @ 'NOW'}",
            workspace_params.clone(),
            ScriptMutability::Immutable,
        ).map_err(|e| anyhow::anyhow!("check import references: {:?}", e))?.rows
            .into_iter()
            .filter(|row| import_references_symbol(&dv_str(&row[1]), entity_name))
            .collect::<Vec<_>>();

        // AST edges: check if any node references this symbol
        let ast_refs = self.run_script(
            "?[from_node, edge_type] := *ast_edge{workspace: $ws, from_node, to_node: $ent, edge_type @ 'NOW'}",
            params.clone(),
            ScriptMutability::Immutable,
        ).map_err(|e| anyhow::anyhow!("check ast references: {:?}", e))?.rows;

        // Call graph: check if any caller targets this symbol or its short method alias.
        let symbol_aliases = symbol_lookup_aliases(entity_name);
        let call_refs = self.run_script(
            "?[caller, callee, file_path, line, context] := *calls_symbol{workspace: $ws, caller, callee, file_path, line, context @ 'NOW'}",
            workspace_params.clone(),
            ScriptMutability::Immutable,
        ).map_err(|e| anyhow::anyhow!("check call references: {:?}", e))?.rows
            .into_iter()
            .filter(|row| {
                (context.is_empty() || dv_str(&row[4]) == context)
                    && symbol_aliases.iter().any(|alias| dv_str(&row[1]) == *alias)
            })
            .collect::<Vec<_>>();

        let field_type_refs = self.run_script(
            "?[context, owner_kind, owner, name, field_type] := *field{workspace: $ws, context, owner_kind, owner, name, field_type @ 'NOW'}",
            workspace_params.clone(),
            ScriptMutability::Immutable,
        ).map_err(|e| anyhow::anyhow!("check field type references: {:?}", e))?.rows
            .into_iter()
            .filter(|row| type_references_symbol(&dv_str(&row[4]), entity_name))
            .collect::<Vec<_>>();
        let method_return_refs = self.run_script(
            "?[context, owner_kind, owner, name, return_type] := *method{workspace: $ws, context, owner_kind, owner, name, return_type @ 'NOW'}",
            workspace_params.clone(),
            ScriptMutability::Immutable,
        ).map_err(|e| anyhow::anyhow!("check method return references: {:?}", e))?.rows
            .into_iter()
            .filter(|row| type_references_symbol(&dv_str(&row[4]), entity_name))
            .collect::<Vec<_>>();
        let method_param_refs = self.run_script(
            "?[context, owner_kind, owner, method, name, param_type] := *method_param{workspace: $ws, context, owner_kind, owner, method, name, param_type @ 'NOW'}",
            workspace_params.clone(),
            ScriptMutability::Immutable,
        ).map_err(|e| anyhow::anyhow!("check method parameter references: {:?}", e))?.rows
            .into_iter()
            .filter(|row| type_references_symbol(&dv_str(&row[5]), entity_name))
            .collect::<Vec<_>>();

        let has_symbol_refs =
            !import_refs.is_empty() || !ast_refs.is_empty() || !call_refs.is_empty();
        let has_type_refs = !field_type_refs.is_empty()
            || !method_return_refs.is_empty()
            || !method_param_refs.is_empty();

        Ok(serde_json::json!({
            "can_delete": !has_deps && !has_symbol_refs && !has_type_refs,
            "aggregates_referencing": aggreg.rows.iter().map(|r| dv_str(&r[1])).collect::<Vec<_>>(),
            "events_sourced": events.rows.iter().map(|r| dv_str(&r[1])).collect::<Vec<_>>(),
            "repositories_managing": repos.rows.iter().map(|r| dv_str(&r[1])).collect::<Vec<_>>(),
            "aggregate_references": aggreg.rows.iter().map(|r| json!({
                "context": dv_str(&r[0]),
                "aggregate": dv_str(&r[1]),
            })).collect::<Vec<_>>(),
            "event_references": events.rows.iter().map(|r| json!({
                "context": dv_str(&r[0]),
                "event": dv_str(&r[1]),
                "file": dv_str(&r[2]),
                "start_line": dv_i64(&r[3]),
                "end_line": dv_i64(&r[4]),
            })).collect::<Vec<_>>(),
            "repository_references": repos.rows.iter().map(|r| json!({
                "context": dv_str(&r[0]),
                "repository": dv_str(&r[1]),
                "file": dv_str(&r[2]),
                "start_line": dv_i64(&r[3]),
                "end_line": dv_i64(&r[4]),
            })).collect::<Vec<_>>(),
            "import_references": import_refs.iter().map(|r| json!({"file": dv_str(&r[0]), "import": dv_str(&r[1]), "context": dv_str(&r[2])})).collect::<Vec<_>>(),
            "ast_references": ast_refs.iter().map(|r| json!({"from": dv_str(&r[0]), "edge_type": dv_str(&r[1])})).collect::<Vec<_>>(),
            "call_references": call_refs.iter().map(|r| json!({"caller": dv_str(&r[0]), "callee": dv_str(&r[1]), "file": dv_str(&r[2]), "line": dv_i64(&r[3]), "context": dv_str(&r[4])})).collect::<Vec<_>>(),
            "type_references": {
                "fields": field_type_refs.iter().map(|r| json!({"context": dv_str(&r[0]), "owner_kind": dv_str(&r[1]), "owner": dv_str(&r[2]), "field": dv_str(&r[3]), "field_type": dv_str(&r[4])})).collect::<Vec<_>>(),
                "method_returns": method_return_refs.iter().map(|r| json!({"context": dv_str(&r[0]), "owner_kind": dv_str(&r[1]), "owner": dv_str(&r[2]), "method": dv_str(&r[3]), "return_type": dv_str(&r[4])})).collect::<Vec<_>>(),
                "method_params": method_param_refs.iter().map(|r| json!({"context": dv_str(&r[0]), "owner_kind": dv_str(&r[1]), "owner": dv_str(&r[2]), "method": dv_str(&r[3]), "param": dv_str(&r[4]), "param_type": dv_str(&r[5])})).collect::<Vec<_>>(),
            },
        }))
    }

    // ── Call Graph Queries ────────────────────────────────────────────────

    /// Return all direct callers of a symbol.
    pub fn call_graph_callers(&self, workspace: &str, symbol: &str) -> Result<serde_json::Value> {
        let ws = canonicalize_path(workspace);
        let params = params_map(&[("ws", &ws)]);
        let rows = self.run_script(
            "?[caller, callee, file_path, line, context] := *calls_symbol{workspace: $ws, caller, callee, state: 'actual', file_path, line, context @ 'NOW'}",
            params,
            ScriptMutability::Immutable,
        ).map_err(|e| anyhow::anyhow!("call_graph_callers: {:?}", e))?;
        let callers = rows
            .rows
            .iter()
            .filter(|row| symbol_lookup_matches(&dv_str(&row[1]), symbol))
            .map(|r| {
                json!({
                    "caller": dv_str(&r[0]),
                    "callee": dv_str(&r[1]),
                    "file": dv_str(&r[2]),
                    "line": dv_i64(&r[3]),
                    "context": dv_str(&r[4]),
                })
            })
            .collect::<Vec<_>>();
        let count = callers.len();
        Ok(json!({
            "symbol": symbol,
            "callers": callers,
            "count": count,
        }))
    }

    /// Return all direct callees of a symbol.
    pub fn call_graph_callees(&self, workspace: &str, symbol: &str) -> Result<serde_json::Value> {
        let ws = canonicalize_path(workspace);
        let params = params_map(&[("ws", &ws)]);
        let rows = self.run_script(
            "?[caller, callee, file_path, line, context] := *calls_symbol{workspace: $ws, caller, callee, state: 'actual', file_path, line, context @ 'NOW'}",
            params,
            ScriptMutability::Immutable,
        ).map_err(|e| anyhow::anyhow!("call_graph_callees: {:?}", e))?;
        let callees = rows
            .rows
            .iter()
            .filter(|row| symbol_lookup_matches(&dv_str(&row[0]), symbol))
            .map(|r| {
                json!({
                    "caller": dv_str(&r[0]),
                    "callee": dv_str(&r[1]),
                    "file": dv_str(&r[2]),
                    "line": dv_i64(&r[3]),
                    "context": dv_str(&r[4]),
                })
            })
            .collect::<Vec<_>>();
        let count = callees.len();
        Ok(json!({
            "symbol": symbol,
            "callees": callees,
            "count": count,
        }))
    }

    /// Compute transitive call reachability from a symbol using Datalog fixed-point.
    pub fn call_graph_reachability(
        &self,
        workspace: &str,
        symbol: &str,
    ) -> Result<serde_json::Value> {
        let ws = canonicalize_path(workspace);
        let params = params_map(&[("ws", &ws)]);
        let rows = self.run_script(
            "?[caller, callee] := *calls_symbol{workspace: $ws, caller, callee, state: 'actual' @ 'NOW'}",
            params,
            ScriptMutability::Immutable,
        ).map_err(|e| anyhow::anyhow!("call_graph_reachability: {:?}", e))?;
        let mut adjacency: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut starts = Vec::new();
        for row in &rows.rows {
            let caller = dv_str(&row[0]);
            let callee = dv_str(&row[1]);
            if symbol_lookup_matches(&caller, symbol) {
                starts.push(caller.clone());
            }
            adjacency.entry(caller).or_default().push(callee);
        }
        for targets in adjacency.values_mut() {
            targets.sort();
            targets.dedup();
        }
        starts.sort();
        starts.dedup();

        let mut reachable = BTreeSet::new();
        let mut stack = starts;
        let mut visited = BTreeSet::new();
        while let Some(current) = stack.pop() {
            if !visited.insert(current.clone()) {
                continue;
            }
            if let Some(callees) = adjacency.get(&current) {
                for callee in callees {
                    if reachable.insert(callee.clone()) {
                        stack.push(callee.clone());
                    }
                }
            }
        }
        let reachable = reachable.into_iter().collect::<Vec<_>>();
        let count = reachable.len();
        Ok(json!({
            "symbol": symbol,
            "reachable": reachable,
            "count": count,
        }))
    }

    /// Summary statistics for the call graph in a workspace.
    pub fn call_graph_stats(&self, workspace: &str) -> Result<serde_json::Value> {
        let ws = canonicalize_path(workspace);
        let params = params_map(&[("ws", &ws)]);

        let total = self.run_script(
            "?[count(caller)] := *calls_symbol{workspace: $ws, caller, state: 'actual' @ 'NOW'}",
            params.clone(),
            ScriptMutability::Immutable,
        ).map_err(|e| anyhow::anyhow!("call_graph_stats total: {:?}", e))?;

        let unique_callers = self.run_script(
            "?[count_unique(caller)] := *calls_symbol{workspace: $ws, caller, state: 'actual' @ 'NOW'}",
            params.clone(),
            ScriptMutability::Immutable,
        ).map_err(|e| anyhow::anyhow!("call_graph_stats callers: {:?}", e))?;

        let unique_callees = self.run_script(
            "?[count_unique(callee)] := *calls_symbol{workspace: $ws, callee, state: 'actual' @ 'NOW'}",
            params.clone(),
            ScriptMutability::Immutable,
        ).map_err(|e| anyhow::anyhow!("call_graph_stats callees: {:?}", e))?;

        // Top-10 most-called symbols
        let hot_callees = self.run_script(
            "?[callee, count(caller)] := *calls_symbol{workspace: $ws, caller, callee, state: 'actual' @ 'NOW'} \
             :order -count(caller) \
             :limit 10",
            params.clone(),
            ScriptMutability::Immutable,
        ).map_err(|e| anyhow::anyhow!("call_graph_stats hot: {:?}", e))?;

        let call_callees = self.run_script(
            "?[caller, callee, file_path, line] := *calls_symbol{workspace: $ws, caller, callee, state: 'actual', file_path, line @ 'NOW'}",
            params.clone(),
            ScriptMutability::Immutable,
        ).map_err(|e| anyhow::anyhow!("call_graph_stats callee rows: {:?}", e))?;
        let symbols = self
            .run_script(
                "?[name] := *symbol{workspace: $ws, name, state: 'actual' @ 'NOW'}",
                params.clone(),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("call_graph_stats symbols: {:?}", e))?;
        let resolved_call_callees = self
            .run_script(
                "?[caller, callee, callee_file, callee_line, caller_file, caller_line, call_site_line, call_expr, dispatch_kind] := *resolved_call{workspace: $ws, caller, callee, callee_file, state: 'actual', callee_line, caller_file, caller_line, call_site_line, call_expr, dispatch_kind @ 'NOW'}",
                params,
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("call_graph_stats resolved callees: {:?}", e))?;

        let mut symbol_aliases: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut symbol_names = BTreeSet::new();
        for row in &symbols.rows {
            let symbol = dv_str(&row[0]);
            symbol_names.insert(symbol.clone());
            for alias in symbol_lookup_aliases(&symbol) {
                symbol_aliases
                    .entry(alias)
                    .or_default()
                    .insert(symbol.clone());
            }
        }
        let mut project_callee_counts: BTreeMap<String, (i64, BTreeSet<String>)> = BTreeMap::new();
        for row in &call_callees.rows {
            let callee = dv_str(&row[1]);
            if let Some(matched_symbols) = symbol_aliases.get(&callee) {
                let entry = project_callee_counts
                    .entry(callee)
                    .or_insert_with(|| (0, BTreeSet::new()));
                entry.0 += 1;
                entry.1.extend(matched_symbols.iter().cloned());
            }
        }
        let project_callee_edges = project_callee_counts
            .values()
            .map(|(count, _)| *count)
            .sum::<i64>();
        let unique_project_callees = project_callee_counts.len();
        let mut hot_project_callees = project_callee_counts.into_iter().collect::<Vec<_>>();
        hot_project_callees
            .sort_by(|left, right| right.1.0.cmp(&left.1.0).then_with(|| left.0.cmp(&right.0)));
        hot_project_callees.truncate(10);

        let mut resolved_project_callee_counts: BTreeMap<(String, String, i64), i64> =
            BTreeMap::new();
        for row in &resolved_call_callees.rows {
            let callee = dv_str(&row[1]);
            if !symbol_names.contains(&callee) {
                continue;
            }
            let callee_file = dv_str(&row[2]);
            let callee_line = dv_i64(&row[3]);
            *resolved_project_callee_counts
                .entry((callee, callee_file, callee_line))
                .or_default() += 1;
        }
        let resolved_project_callee_edges = resolved_project_callee_counts.values().sum::<i64>();
        let unique_resolved_project_callees = resolved_project_callee_counts.len();
        let mut hot_resolved_project_callees = resolved_project_callee_counts
            .into_iter()
            .collect::<Vec<_>>();
        hot_resolved_project_callees.sort_by(|left, right| {
            right
                .1
                .cmp(&left.1)
                .then_with(|| left.0.0.cmp(&right.0.0))
                .then_with(|| left.0.1.cmp(&right.0.1))
                .then_with(|| left.0.2.cmp(&right.0.2))
        });
        hot_resolved_project_callees.truncate(10);

        Ok(json!({
            "total_edges": if total.rows.is_empty() { 0 } else { dv_i64(&total.rows[0][0]) },
            "unique_callers": if unique_callers.rows.is_empty() { 0 } else { dv_i64(&unique_callers.rows[0][0]) },
            "unique_callees": if unique_callees.rows.is_empty() { 0 } else { dv_i64(&unique_callees.rows[0][0]) },
            "hottest_callees": hot_callees.rows.iter().map(|r| json!({
                "callee": dv_str(&r[0]),
                "call_count": dv_i64(&r[1]),
            })).collect::<Vec<_>>(),
            "project_callee_edges": project_callee_edges,
            "unique_project_callees": unique_project_callees,
            "project_callee_stats": {
                "call_graph_relation": "calls_symbol",
                "ambiguity": "name_based",
            },
            "hottest_project_callees": hot_project_callees.into_iter().map(|(callee, (count, matched_symbols))| json!({
                "callee": callee,
                "call_count": count,
                "call_graph_relation": "calls_symbol",
                "ambiguity": "name_based",
                "matched_symbols": matched_symbols.into_iter().collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "resolved_project_callee_edges": resolved_project_callee_edges,
            "unique_resolved_project_callees": unique_resolved_project_callees,
            "resolved_project_callee_stats": {
                "call_graph_relation": "resolved_call",
                "ambiguity": "compiler_resolved",
            },
            "hottest_resolved_project_callees": hot_resolved_project_callees.into_iter().map(|((callee, callee_file, callee_line), count)| json!({
                "callee": callee,
                "callee_file": callee_file,
                "callee_line": callee_line,
                "call_count": count,
                "call_graph_relation": "resolved_call",
                "provenance": "callee definition; resolved_call rows also persist caller_file, caller_line, call_site_line, call_expr, and dispatch_kind",
            })).collect::<Vec<_>>(),
        }))
    }

    /// Infer refactoring and optimization shape candidates from the stored Rust graph.
    ///
    /// These are evidence-backed candidates, not automatic edits: the static graph can
    /// identify coupling, fan-in/fan-out, and ambiguity, while runtime speedups still
    /// need profiling or benchmarks to confirm.
    pub fn optimization_recommendations(&self, workspace: &str) -> Result<serde_json::Value> {
        self.optimization_recommendations_scoped(workspace, "all")
    }

    pub fn optimization_recommendations_scoped(
        &self,
        workspace: &str,
        scope: &str,
    ) -> Result<serde_json::Value> {
        let requested_scope = RustFactScope::parse(scope, RustFactScope::All)?;
        let ws = canonicalize_path(workspace);
        let params = params_map(&[("ws", &ws)]);

        let source_files = self
            .run_script(
                "?[path, context] := *source_file{workspace: $ws, path, state: 'actual', context @ 'NOW'}",
                params.clone(),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("optimization source_files: {:?}", e))?;
        let modules = self
            .run_script(
                "?[context, name, path, public, file_path] := *module{workspace: $ws, context, name, state: 'actual', path, public, file_path @ 'NOW'}",
                params.clone(),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("optimization modules: {:?}", e))?;
        let symbols = self
            .run_script(
                "?[name, kind, context, file_path, visibility] := *symbol{workspace: $ws, name, state: 'actual', kind, context, file_path, visibility @ 'NOW'}",
                params.clone(),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("optimization symbols: {:?}", e))?;
        let imports = self
            .run_script(
                "?[from_file, to_module, context] := *import_edge{workspace: $ws, from_file, to_module, state: 'actual', context @ 'NOW'}",
                params.clone(),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("optimization imports: {:?}", e))?;
        let calls = self
            .run_script(
                "?[caller, callee, file_path, context] := *calls_symbol{workspace: $ws, caller, callee, state: 'actual', file_path, context @ 'NOW'}",
                params.clone(),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("optimization calls: {:?}", e))?;
        let resolved_calls = self
            .run_script(
                "?[caller, callee, callee_file, caller_file] := *resolved_call{workspace: $ws, caller, callee, callee_file, caller_file, state: 'actual' @ 'NOW'}",
                params.clone(),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("optimization resolved_calls: {:?}", e))?;
        let ast_edges = self
            .run_script(
                "?[from_node, to_node, edge_type, file_path] := *ast_edge{workspace: $ws, from_node, to_node, edge_type, state: 'actual', file_path @ 'NOW'}",
                params,
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("optimization ast_edges: {:?}", e))?;

        let mut known_modules: BTreeSet<String> = BTreeSet::new();
        for row in &source_files.rows {
            let path = dv_str(&row[0]);
            if !rust_fact_allowed(requested_scope, &path, "", "") {
                continue;
            }
            let module = file_module_path(&path);
            if !module.is_empty() {
                known_modules.insert(module);
            }
        }
        for row in &symbols.rows {
            let name = dv_str(&row[0]);
            let file_path = dv_str(&row[3]);
            if !rust_fact_allowed(requested_scope, &file_path, &name, "") {
                continue;
            }
            let module = file_module_path(&file_path);
            if !module.is_empty() {
                known_modules.insert(module);
            }
        }
        for row in &modules.rows {
            let module_path = dv_str(&row[2]);
            let file_path = dv_str(&row[4]);
            if !module_path.is_empty()
                && rust_fact_allowed(requested_scope, &file_path, &module_path, "")
            {
                known_modules.insert(module_path);
            }
        }
        let mut project_call_aliases: BTreeSet<String> = BTreeSet::new();
        for row in &symbols.rows {
            let name = dv_str(&row[0]);
            let file_path = dv_str(&row[3]);
            if !rust_fact_allowed(requested_scope, &file_path, &name, "") {
                continue;
            }
            for alias in symbol_lookup_aliases(&name) {
                project_call_aliases.insert(alias);
            }
        }

        let mut recommendations = Vec::new();

        // lib.rs/mod.rs are already Rust facade surfaces. Optimization here is
        // hardening: tighten child-module visibility, make re-exports explicit,
        // and route deep import bypasses back through the facade.
        let mut symbols_by_file: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
        for row in &symbols.rows {
            let name = dv_str(&row[0]);
            let file_path = dv_str(&row[3]);
            if !rust_fact_allowed(requested_scope, &file_path, &name, "") {
                continue;
            }
            symbols_by_file
                .entry(file_path.clone())
                .or_default()
                .push(json!({
                    "name": name,
                    "kind": dv_str(&row[1]),
                    "context": dv_str(&row[2]),
                    "visibility": dv_str(&row[4]),
                }));
        }
        let mut modules_by_decl_file: BTreeMap<String, Vec<(String, bool)>> = BTreeMap::new();
        for row in &modules.rows {
            let module_path = dv_str(&row[2]);
            let file_path = dv_str(&row[4]);
            if !rust_fact_allowed(requested_scope, &file_path, &module_path, "") {
                continue;
            }
            modules_by_decl_file
                .entry(file_path)
                .or_default()
                .push((module_path, matches!(&row[3], cozo::DataValue::Bool(true))));
        }
        let mut resolved_imports_by_file: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut deep_importers_by_module: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut module_import_nodes: BTreeSet<String> = BTreeSet::new();
        let mut module_import_edges: BTreeSet<(String, String)> = BTreeSet::new();
        let mut import_files_by_edge: BTreeMap<(String, String), BTreeSet<String>> =
            BTreeMap::new();
        let mut import_paths_by_edge: BTreeMap<(String, String), BTreeSet<String>> =
            BTreeMap::new();
        for row in &imports.rows {
            let from_file = dv_str(&row[0]);
            let to_module = dv_str(&row[1]);
            if !rust_fact_allowed(requested_scope, &from_file, "", &to_module) {
                continue;
            }
            let from_module = file_module_path(&from_file);
            let Some(target_module) =
                resolve_internal_module(&to_module, &from_module, &known_modules)
            else {
                continue;
            };
            resolved_imports_by_file
                .entry(from_file.clone())
                .or_default()
                .insert(target_module.clone());
            if from_module != target_module && !is_ancestor(&target_module, &from_module) {
                deep_importers_by_module
                    .entry(target_module.clone())
                    .or_default()
                    .insert(from_file.clone());
            }
            if from_module != target_module
                && !is_ancestor(&target_module, &from_module)
                && !is_ancestor(&from_module, &target_module)
            {
                module_import_nodes.insert(from_module.clone());
                module_import_nodes.insert(target_module.clone());
                let edge = (from_module, target_module);
                module_import_edges.insert(edge.clone());
                import_files_by_edge
                    .entry(edge.clone())
                    .or_default()
                    .insert(from_file);
                import_paths_by_edge
                    .entry(edge)
                    .or_default()
                    .insert(to_module);
            }
        }
        for row in &source_files.rows {
            let file_path = dv_str(&row[0]);
            if !rust_fact_allowed(requested_scope, &file_path, "", "") {
                continue;
            }
            let Some(surface_kind) = rust_surface_file_kind(&file_path) else {
                continue;
            };
            let declared_modules = modules_by_decl_file
                .get(&file_path)
                .cloned()
                .unwrap_or_default();
            let public_modules = declared_modules
                .iter()
                .filter_map(|(module, public)| public.then_some(module.clone()))
                .collect::<Vec<_>>();
            let surface_imports = resolved_imports_by_file
                .get(&file_path)
                .cloned()
                .unwrap_or_default();
            let mut deep_importers = BTreeSet::new();
            for module in public_modules.iter().chain(surface_imports.iter()) {
                if let Some(importers) = deep_importers_by_module.get(module) {
                    deep_importers
                        .extend(importers.iter().filter(|path| *path != &file_path).cloned());
                }
            }
            let surface_symbols = symbols_by_file.get(&file_path).cloned().unwrap_or_default();
            if surface_symbols.len() > 1 {
                continue;
            }
            if public_modules.len() < 2 && surface_imports.is_empty() && deep_importers.len() < 2 {
                continue;
            }
            recommendations.push(json!({
                "kind": "facade_surface_hardening",
                "target": file_path,
                "score": usize_to_i64(public_modules.len())
                    + usize_to_i64(surface_imports.len())
                    + usize_to_i64(deep_importers.len()),
                "confidence": if deep_importers.len() >= 2 { "medium" } else { "low" },
                "rationale": "This lib.rs/mod.rs is already a Rust facade surface; harden it so public exposure is deliberate instead of letting child modules become the API by default.",
                "proposed_shape": "Prefer private or pub(crate) child modules plus explicit pub use re-exports for the intended API surface; have downstream imports target this facade rather than deep implementation modules.",
                "evidence": {
                    "facade_role": "existing_rust_surface",
                    "surface_kind": surface_kind,
                    "declared_modules": declared_modules.into_iter().map(|(module, public)| json!({ "module": module, "public": public })).collect::<Vec<_>>(),
                    "public_declared_modules": public_modules,
                    "surface_imports_or_reexports": surface_imports.into_iter().collect::<Vec<_>>(),
                    "deep_importing_files": deep_importers.into_iter().collect::<Vec<_>>(),
                    "local_symbols": surface_symbols,
                    "hardening_actions": ["tighten_visibility", "explicit_pub_use", "route_deep_imports_through_facade"],
                },
                "validation": ["Inspect the lib.rs/mod.rs facade", "Replace broad pub mod exposure with explicit pub use where appropriate", "Run cargo test and rust_graph neighborhood for the facade module"],
            }));
        }

        // Import graph reduction: normalize concrete imports to module edges,
        // then look for graph rewrites that reduce coupling without changing
        // runtime behavior: SCC condensation, transitive reduction, and surface
        // re-export/facade aggregation.
        let mut adjacency: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (from, to) in &module_import_edges {
            adjacency
                .entry(from.clone())
                .or_default()
                .insert(to.clone());
        }
        let cycle_clusters = strongly_connected_module_components(
            module_import_nodes.iter().cloned().collect::<Vec<_>>(),
            &module_import_edges,
        );
        let mut redundant_direct_edges = Vec::new();
        for (from, to) in &module_import_edges {
            if module_path_reachable_without_edge(&adjacency, from, to) {
                let edge = (from.clone(), to.clone());
                redundant_direct_edges.push(json!({
                    "from": from,
                    "to": to,
                    "importing_files": import_files_by_edge
                        .get(&edge)
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .collect::<Vec<_>>(),
                    "import_paths": import_paths_by_edge
                        .get(&edge)
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .collect::<Vec<_>>(),
                }));
            }
        }
        redundant_direct_edges.sort_by(|left, right| {
            left["from"]
                .as_str()
                .unwrap_or("")
                .cmp(right["from"].as_str().unwrap_or(""))
                .then_with(|| {
                    left["to"]
                        .as_str()
                        .unwrap_or("")
                        .cmp(right["to"].as_str().unwrap_or(""))
                })
        });
        let redundant_direct_edge_count = redundant_direct_edges.len();
        redundant_direct_edges.truncate(10);

        let mut surface_children: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut surface_import_files: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut surface_edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for ((from, target), files) in &import_files_by_edge {
            let Some(surface) = module_parent_path(target) else {
                continue;
            };
            if !known_modules.contains(&surface) || *from == surface {
                continue;
            }
            surface_children
                .entry(surface.clone())
                .or_default()
                .insert(target.clone());
            surface_import_files
                .entry(surface.clone())
                .or_default()
                .extend(files.iter().cloned());
            surface_edges
                .entry(surface)
                .or_default()
                .insert(format!("{from}->{target}"));
        }
        let mut surface_opportunities = surface_children
            .into_iter()
            .filter_map(|(surface, children)| {
                let importing_files = surface_import_files.remove(&surface).unwrap_or_default();
                let edges = surface_edges.remove(&surface).unwrap_or_default();
                if importing_files.len() < 3 && children.len() < 2 {
                    return None;
                }
                let score = importing_files.len() + children.len() + edges.len();
                Some((
                    score,
                    json!({
                        "surface": surface,
                        "child_targets": children.into_iter().collect::<Vec<_>>(),
                        "importing_files": importing_files.into_iter().collect::<Vec<_>>(),
                        "collapsible_edges": edges.into_iter().collect::<Vec<_>>(),
                        "candidate_rewrite_count": score,
                    }),
                ))
            })
            .collect::<Vec<_>>();
        surface_opportunities.sort_by(|left, right| {
            right.0.cmp(&left.0).then_with(|| {
                left.1["surface"]
                    .as_str()
                    .unwrap_or("")
                    .cmp(right.1["surface"].as_str().unwrap_or(""))
            })
        });
        let surface_opportunity_score = surface_opportunities
            .iter()
            .map(|(score, _)| *score)
            .sum::<usize>();
        let surface_reexport_opportunities = surface_opportunities
            .into_iter()
            .take(5)
            .map(|(_, opportunity)| opportunity)
            .collect::<Vec<_>>();

        let edge_betweenness =
            module_import_edge_betweenness(&module_import_nodes, &module_import_edges);
        let edge_betweenness_score = edge_betweenness
            .iter()
            .filter(|(_, score)| *score >= 2.0)
            .count();
        let top_edge_betweenness = edge_betweenness
            .iter()
            .take(10)
            .map(|((from, to), score)| {
                let edge = (from.clone(), to.clone());
                json!({
                    "from": from,
                    "to": to,
                    "score": score,
                    "importing_files": import_files_by_edge
                        .get(&edge)
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .collect::<Vec<_>>(),
                    "import_paths": import_paths_by_edge
                        .get(&edge)
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .collect::<Vec<_>>(),
                })
            })
            .collect::<Vec<_>>();
        let separator_modules =
            module_articulation_separators(&module_import_nodes, &module_import_edges)
                .into_iter()
                .map(|(module, separated_components)| {
                    json!({
                        "module": module,
                        "component_count_after_removal": separated_components.len(),
                        "separated_components": separated_components,
                    })
                })
                .collect::<Vec<_>>();
        let separator_module_count = separator_modules.len();
        let separator_edges = module_bridge_separators(&module_import_nodes, &module_import_edges)
            .into_iter()
            .map(|((left, right), separated_components)| {
                let forward_edge = (left.clone(), right.clone());
                let reverse_edge = (right.clone(), left.clone());
                let mut importing_files = BTreeSet::new();
                if let Some(files) = import_files_by_edge.get(&forward_edge) {
                    importing_files.extend(files.iter().cloned());
                }
                if let Some(files) = import_files_by_edge.get(&reverse_edge) {
                    importing_files.extend(files.iter().cloned());
                }
                let mut import_paths = BTreeSet::new();
                if let Some(paths) = import_paths_by_edge.get(&forward_edge) {
                    import_paths.extend(paths.iter().cloned());
                }
                if let Some(paths) = import_paths_by_edge.get(&reverse_edge) {
                    import_paths.extend(paths.iter().cloned());
                }
                json!({
                    "between": [left, right],
                    "component_count_after_removal": separated_components.len(),
                    "separated_components": separated_components,
                    "importing_files": importing_files.into_iter().collect::<Vec<_>>(),
                    "import_paths": import_paths.into_iter().collect::<Vec<_>>(),
                })
            })
            .collect::<Vec<_>>();
        let separator_edge_count = separator_edges.len();

        let import_instance_count = import_files_by_edge
            .values()
            .map(BTreeSet::len)
            .sum::<usize>();
        let reduction_score = redundant_direct_edge_count * 2
            + cycle_clusters.iter().map(Vec::len).sum::<usize>() * 3
            + surface_opportunity_score
            + edge_betweenness_score
            + separator_module_count * 3
            + separator_edge_count * 2;
        if !module_import_edges.is_empty()
            && (reduction_score >= 3
                || module_import_edges.len() >= 6
                || import_instance_count >= 8)
        {
            recommendations.push(json!({
                "kind": "import_graph_reduction",
                "target": "module import graph",
                "score": usize_to_i64(reduction_score.max(module_import_edges.len())),
                "confidence": if redundant_direct_edge_count > 0 || !cycle_clusters.is_empty() { "medium" } else { "low" },
                "rationale": "The normalized import graph can be reduced with graph techniques: condense strongly-connected module clusters, remove direct imports already implied by alternate paths, identify high-betweenness pressure edges, and aggregate deep imports behind explicit facade surfaces or separator modules.",
                "proposed_shape": "Map raw imports to module edges, reduce SCCs to boundary seams, remove transitive direct imports where an owned path already exists, turn high-betweenness or separator modules into explicit ports/facades, and expose deliberate pub use re-exports from existing lib.rs/mod.rs facade surfaces.",
                "evidence": {
                    "techniques": ["scc_condensation", "transitive_reduction", "surface_reexport_aggregation", "edge_betweenness", "separator_analysis"],
                    "module_nodes": module_import_nodes.len(),
                    "unique_module_edges": module_import_edges.len(),
                    "import_instances": import_instance_count,
                    "estimated_unique_edges_after_transitive_reduction": module_import_edges.len().saturating_sub(redundant_direct_edge_count),
                    "cycle_clusters": cycle_clusters,
                    "redundant_direct_edge_count": redundant_direct_edge_count,
                    "redundant_direct_edges": redundant_direct_edges,
                    "surface_reexport_opportunities": surface_reexport_opportunities,
                    "top_edge_betweenness": top_edge_betweenness,
                    "separator_modules": separator_modules,
                    "separator_edges": separator_edges,
                },
                "validation": ["Inspect rust_graph relation=import_edge for the listed modules", "Introduce explicit pub use exports at the proposed surfaces", "Run cargo test and compare rust_history mode=latest_diff"],
            }));
        }

        // Facade candidates: several consumers import the same internal module directly.
        let mut imports_by_target: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for row in &imports.rows {
            let from_file = dv_str(&row[0]);
            if !rust_fact_allowed(requested_scope, &from_file, "", &dv_str(&row[1])) {
                continue;
            }
            let from_module = file_module_path(&from_file);
            let to_module = dv_str(&row[1]);
            let Some(target_module) =
                resolve_internal_module(&to_module, &from_module, &known_modules)
            else {
                continue;
            };
            if is_root_facade_import(&to_module, &target_module) {
                continue;
            }
            if target_module == from_module
                || is_ancestor(&target_module, &from_module)
                || is_ancestor(&from_module, &target_module)
            {
                continue;
            }
            imports_by_target
                .entry(target_module)
                .or_default()
                .insert(from_file);
        }
        for (target, importers) in imports_by_target {
            if importers.len() < 3 {
                continue;
            }
            let public_api_like = public_api_like_module(&target);
            recommendations.push(json!({
                "kind": if public_api_like { "public_api_surface" } else { "facade" },
                "target": target,
                "score": if public_api_like { (usize_to_i64(importers.len())).max(1) / 2 } else { usize_to_i64(importers.len()) },
                "confidence": if public_api_like { "medium" } else if importers.len() >= 5 { "high" } else { "medium" },
                "rationale": if public_api_like { "Several files import a shared public API-shaped module; keep the surface deliberate and avoid broad wildcard coupling." } else { "Several outside files import this internal module directly; a stable facade can reduce downstream churn." },
                "proposed_shape": if public_api_like { "Keep the module as an explicit public API, and tighten imports or split exports if the surface becomes too broad." } else { "Expose a narrow module facade or port and route external imports through it." },
                "evidence": {
                    "importing_files": importers.into_iter().collect::<Vec<_>>(),
                    "public_api_like": public_api_like,
                },
                "validation": ["Run rust_graph neighborhood for the target", "Apply the import rewrite", "Run cargo test and compare rust_history mode=latest_diff"],
            }));
        }

        // Call fan-in candidates: a single callee acts as a coordination, actor, or reduce point.
        let mut callers_by_callee: BTreeMap<String, BTreeMap<String, BTreeSet<String>>> =
            BTreeMap::new();
        let mut contexts_by_callee: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for row in &calls.rows {
            let caller = dv_str(&row[0]);
            let callee = dv_str(&row[1]);
            let file_path = dv_str(&row[2]);
            if !rust_fact_allowed(requested_scope, &file_path, &caller, &callee) {
                continue;
            }
            let context = dv_str(&row[3]);
            callers_by_callee
                .entry(callee.clone())
                .or_default()
                .entry(caller)
                .or_default()
                .insert(file_path);
            if !context.is_empty() {
                contexts_by_callee
                    .entry(callee)
                    .or_default()
                    .insert(context);
            }
        }
        for (callee, caller_files) in &callers_by_callee {
            let caller_count = caller_files.len();
            if caller_count < 3 {
                continue;
            }
            let files: BTreeSet<String> = caller_files
                .values()
                .flat_map(|files| files.iter().cloned())
                .collect();
            let contexts = contexts_by_callee.get(callee).cloned().unwrap_or_default();
            let callee_lower = callee.to_lowercase();
            let project_owned_callee = project_call_aliases.contains(callee);
            let qualified_callee = callee.contains("::");
            if common_non_project_callee(callee) && !project_owned_callee && !qualified_callee {
                continue;
            }
            let is_reduce_like = [
                "collect",
                "merge",
                "reduce",
                "aggregate",
                "batch",
                "extend",
                "insert",
                "save",
                "record",
                "scan",
                "parse",
                "compute",
            ]
            .iter()
            .any(|needle| callee_lower.contains(needle));
            let is_actor_like = [
                "handle",
                "dispatch",
                "process",
                "send",
                "update",
                "upsert",
                "save",
                "record",
                "reload",
                "invalidate",
            ]
            .iter()
            .any(|needle| callee_lower.contains(needle));
            if is_reduce_like && files.len() >= 3 {
                recommendations.push(json!({
                    "kind": "map_reduce",
                    "target": callee,
                    "score": usize_to_i64(caller_count) + usize_to_i64(files.len()),
                    "confidence": if contexts.len() > 1 { "medium" } else { "low" },
                    "rationale": "Many independent callers feed the same aggregation-shaped callee; the call graph suggests a possible map/reduce or batch pipeline boundary.",
                    "proposed_shape": "Map work per file/module, reduce through this callee, and validate semantic independence before parallelizing.",
                    "evidence": {
                        "caller_count": caller_count,
                        "files": files.iter().cloned().collect::<Vec<_>>(),
                        "contexts": contexts.iter().cloned().collect::<Vec<_>>(),
                    },
                    "validation": ["Check side effects and shared state", "Benchmark before and after", "Run cargo test"],
                }));
            }
            if is_actor_like && caller_count >= 4 {
                recommendations.push(json!({
                    "kind": "actor_boundary",
                    "target": callee,
                    "score": usize_to_i64(caller_count),
                    "confidence": "low",
                    "rationale": "A command-shaped callee has broad fan-in; if it owns mutable coordination, it may want an actor or queue boundary.",
                    "proposed_shape": "Keep state behind one owner and have callers submit typed commands/events instead of direct coordination calls.",
                    "evidence": {
                        "caller_count": caller_count,
                        "callers": caller_files.keys().cloned().collect::<Vec<_>>(),
                        "contexts": contexts.iter().cloned().collect::<Vec<_>>(),
                    },
                    "validation": ["Confirm mutable state, locks, or async coordination", "Model command enum/API", "Run concurrency tests"],
                }));
            }
        }

        // Rename candidates: ambiguous short names are risky when call edges use aliases.
        let mut symbols_by_short_name: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
        for row in &symbols.rows {
            let name = dv_str(&row[0]);
            let file_path = dv_str(&row[3]);
            if !rust_fact_allowed(requested_scope, &file_path, &name, "") {
                continue;
            }
            let short = name.rsplit("::").next().unwrap_or(&name).to_string();
            symbols_by_short_name.entry(short).or_default().push(json!({
                "name": name,
                "kind": dv_str(&row[1]),
                "context": dv_str(&row[2]),
                "file": file_path,
                "visibility": dv_str(&row[4]),
            }));
        }
        for (short, matches) in symbols_by_short_name {
            if matches.len() < 2 || matches.len() > 12 {
                continue;
            }
            let ambiguous_calls = calls
                .rows
                .iter()
                .filter(|row| dv_str(&row[1]) == short)
                .count();
            if ambiguous_calls == 0 && !generic_symbol_name(&short) {
                continue;
            }
            recommendations.push(json!({
                "kind": "rename",
                "target": short,
                "score": usize_to_i64(matches.len()) + usize_to_i64(ambiguous_calls),
                "confidence": if ambiguous_calls > 0 { "medium" } else { "low" },
                "rationale": "Multiple symbols share this short name, which makes syntactic call/import evidence ambiguous and weakens future rename safety.",
                "proposed_shape": "Give at least the cross-boundary or high-fan-in symbol a role-specific name, then use rust-analyzer rename for edits.",
                "evidence": {
                    "matching_symbols": matches,
                    "ambiguous_call_edges": ambiguous_calls,
                },
                "validation": ["Run rust_scan for compiler-resolved call edges", "Use rust-analyzer rename", "Run cargo test"],
            }));
        }

        // Move/facade candidates: callers/importers mostly live in another context.
        let mut symbol_context_by_name = BTreeMap::new();
        let mut symbol_file_by_name = BTreeMap::new();
        for row in &symbols.rows {
            let name = dv_str(&row[0]);
            let context = dv_str(&row[2]);
            let file = dv_str(&row[3]);
            if rust_fact_allowed(requested_scope, &file, &name, "") {
                symbol_context_by_name.insert(name.clone(), context);
                symbol_file_by_name.insert(name, file);
            }
        }
        let use_resolved_move_calls = !resolved_calls.rows.is_empty();
        let move_facade_relation = if use_resolved_move_calls {
            "resolved_call"
        } else {
            "calls_symbol"
        };
        let mut symbol_by_alias: BTreeMap<String, Vec<(String, String, String)>> = BTreeMap::new();
        for row in &symbols.rows {
            let name = dv_str(&row[0]);
            let context = dv_str(&row[2]);
            let file = dv_str(&row[3]);
            if !rust_fact_allowed(requested_scope, &file, &name, "") {
                continue;
            }
            for alias in symbol_lookup_aliases(&name) {
                symbol_by_alias.entry(alias).or_default().push((
                    name.clone(),
                    context.clone(),
                    file.clone(),
                ));
            }
        }
        let mut incoming_contexts: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
        if use_resolved_move_calls {
            for row in &resolved_calls.rows {
                let caller = dv_str(&row[0]);
                let callee = dv_str(&row[1]);
                let callee_file = dv_str(&row[2]);
                let caller_file = dv_str(&row[3]);
                if !symbol_context_by_name.contains_key(&callee) {
                    continue;
                }
                let caller_file = if caller_file.is_empty() {
                    let Some(symbol_file) = symbol_file_by_name.get(&caller) else {
                        continue;
                    };
                    symbol_file.as_str()
                } else {
                    caller_file.as_str()
                };
                if !rust_fact_allowed(requested_scope, caller_file, &caller, &callee) {
                    continue;
                }
                if !rust_fact_allowed(requested_scope, &callee_file, &callee, "") {
                    continue;
                }
                let Some(caller_context) = symbol_context_by_name.get(&caller) else {
                    continue;
                };
                if !caller_context.is_empty() {
                    *incoming_contexts
                        .entry(callee)
                        .or_default()
                        .entry(caller_context.clone())
                        .or_default() += 1;
                }
            }
        } else {
            for row in &calls.rows {
                let callee = dv_str(&row[1]);
                let caller = dv_str(&row[0]);
                let file_path = dv_str(&row[2]);
                if !rust_fact_allowed(requested_scope, &file_path, &caller, &callee) {
                    continue;
                }
                let caller_context = dv_str(&row[3]);
                let Some(matches) = symbol_by_alias.get(&callee) else {
                    continue;
                };
                for (symbol, _, _) in matches {
                    if !caller_context.is_empty() {
                        *incoming_contexts
                            .entry(symbol.clone())
                            .or_default()
                            .entry(caller_context.clone())
                            .or_default() += 1;
                    }
                }
            }
        }
        for row in &symbols.rows {
            let symbol = dv_str(&row[0]);
            let declared_context = dv_str(&row[2]);
            let file_path = dv_str(&row[3]);
            if !rust_fact_allowed(requested_scope, &file_path, &symbol, "") {
                continue;
            }
            if declared_context.is_empty() {
                continue;
            }
            let Some(counts) = incoming_contexts.get(&symbol) else {
                continue;
            };
            let total: usize = counts.values().sum();
            if total < 3 {
                continue;
            }
            let Some((dominant_context, dominant_count)) = counts
                .iter()
                .max_by(|left, right| left.1.cmp(right.1).then_with(|| right.0.cmp(left.0)))
            else {
                continue;
            };
            if dominant_context == &declared_context || *dominant_count * 2 < total {
                continue;
            }
            recommendations.push(json!({
                "kind": "move_or_facade",
                "target": symbol,
                "score": usize_to_i64(*dominant_count),
                "confidence": "medium",
                "rationale": "Most inbound calls come from a different context than the symbol's declared context; either the symbol belongs nearer those callers or the current context needs a clearer facade.",
                "proposed_shape": "Evaluate moving the symbol, extracting a port, or exposing an explicit facade from the owning context.",
                "evidence": {
                    "declared_context": declared_context,
                    "dominant_caller_context": dominant_context,
                    "caller_context_counts": counts,
                    "call_graph_relation": move_facade_relation,
                    "total_inbound_calls": total,
                },
                "validation": ["Check ownership semantics before moving", "Use rust_impact on the symbol", "Run cargo test and rust_history mode=latest_diff"],
            }));
        }

        // Trait/adapter candidates: concrete types implement ports used across contexts.
        let impl_edges = ast_edges
            .rows
            .iter()
            .filter(|row| {
                let from_node = dv_str(&row[0]);
                let to_node = dv_str(&row[1]);
                let file_path = dv_str(&row[3]);
                dv_str(&row[2]) == "implements"
                    && rust_fact_allowed(requested_scope, &file_path, &from_node, &to_node)
            })
            .count();
        if impl_edges > 0 {
            recommendations.push(json!({
                "kind": "port_adapter_review",
                "target": "trait implementations",
                "score": usize_to_i64(impl_edges),
                "confidence": "low",
                "rationale": "The graph contains trait implementation edges; cross-context concrete dependencies should usually point at the trait port instead of adapters.",
                "proposed_shape": "Review implements edges with inbound imports/calls and keep adapters behind their port boundary.",
                "evidence": { "implements_edges": impl_edges },
                "validation": ["Query rust_graph relation=ast_edge to inspect implements edges", "Check imports to concrete adapters", "Run cargo test"],
            }));
        }

        recommendations.sort_by(|left, right| {
            right["score"]
                .as_i64()
                .unwrap_or(0)
                .cmp(&left["score"].as_i64().unwrap_or(0))
                .then_with(|| {
                    left["kind"]
                        .as_str()
                        .unwrap_or("")
                        .cmp(right["kind"].as_str().unwrap_or(""))
                })
                .then_with(|| {
                    left["target"]
                        .as_str()
                        .unwrap_or("")
                        .cmp(right["target"].as_str().unwrap_or(""))
                })
        });
        recommendations.truncate(20);

        Ok(json!({
            "analysis": "optimize",
            "scope": requested_scope.as_str(),
            "recommendations": recommendations,
            "count": recommendations.len(),
            "fact_counts": {
                "source_files": source_files.rows.iter().filter(|row| rust_fact_allowed(requested_scope, &dv_str(&row[0]), "", "")).count(),
                "modules": modules.rows.iter().filter(|row| rust_fact_allowed(requested_scope, &dv_str(&row[4]), &dv_str(&row[2]), "")).count(),
                "symbols": symbols.rows.iter().filter(|row| rust_fact_allowed(requested_scope, &dv_str(&row[3]), &dv_str(&row[0]), "")).count(),
                "import_edges": imports.rows.iter().filter(|row| rust_fact_allowed(requested_scope, &dv_str(&row[0]), "", &dv_str(&row[1]))).count(),
                "call_edges": calls.rows.iter().filter(|row| rust_fact_allowed(requested_scope, &dv_str(&row[2]), &dv_str(&row[0]), &dv_str(&row[1]))).count(),
                "resolved_call_edges": resolved_calls.rows.iter().filter(|row| rust_fact_allowed(requested_scope, &dv_str(&row[2]), &dv_str(&row[0]), &dv_str(&row[1]))).count(),
                "ast_edges": ast_edges.rows.iter().filter(|row| rust_fact_allowed(requested_scope, &dv_str(&row[3]), &dv_str(&row[0]), &dv_str(&row[1]))).count(),
            },
            "note": "Static graph recommendations identify refactoring and optimization candidates; runtime wins require profiling or benchmarks.",
        }))
    }

    /// Surface Rust best-practice findings from stored actual-state source facts.
    ///
    /// These findings are triage signals, not lints: they are ranked with graph
    /// evidence so risky constructs near central symbols rise above local noise.
    pub fn practice_findings(&self, workspace: &str) -> Result<serde_json::Value> {
        self.practice_findings_scoped(workspace, "all")
    }

    pub fn practice_findings_scoped(
        &self,
        workspace: &str,
        scope: &str,
    ) -> Result<serde_json::Value> {
        let requested_scope = RustFactScope::parse(scope, RustFactScope::All)?;
        let ws = canonicalize_path(workspace);
        let params = params_map(&[("ws", &ws)]);
        let scoped_priority = |priority_score: i64, scope: &str| -> i64 {
            if scope == "test" {
                priority_score.clamp(10, 25)
            } else {
                priority_score
            }
        };

        let symbols = self
            .run_script(
                "?[name, kind, context, file_path, start_line, end_line, visibility] := *symbol{workspace: $ws, name, state: 'actual', kind, context, file_path, start_line, end_line, visibility @ 'NOW'}",
                params.clone(),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("practice symbols: {:?}", e))?;
        let calls = self
            .run_script(
                "?[caller, callee, file_path, line, context] := *calls_symbol{workspace: $ws, caller, callee, state: 'actual', file_path, line, context @ 'NOW'}",
                params.clone(),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("practice calls: {:?}", e))?;
        let ast_edges = self
            .run_script(
                "?[from_node, to_node, edge_type, file_path, line] := *ast_edge{workspace: $ws, from_node, to_node, edge_type, state: 'actual', file_path, line @ 'NOW'}",
                params.clone(),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("practice ast_edges: {:?}", e))?;
        let references = self
            .run_script(
                "?[from_file, to_path, reference_kind, line, context] := *reference_edge{workspace: $ws, from_file, to_path, reference_kind, line, state: 'actual', context @ 'NOW'}",
                params,
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("practice reference_edges: {:?}", e))?;

        let mut inbound_by_callee: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for row in &calls.rows {
            let caller = dv_str(&row[0]);
            let callee = dv_str(&row[1]);
            let file_path = dv_str(&row[2]);
            if !rust_fact_allowed(requested_scope, &file_path, &caller, &callee) {
                continue;
            }
            inbound_by_callee.entry(callee).or_default().insert(caller);
        }

        let mut findings = Vec::new();

        for row in &ast_edges.rows {
            let from_node = dv_str(&row[0]);
            let directive = dv_str(&row[1]);
            let edge_type = dv_str(&row[2]);
            if edge_type != "decorators" || !directive.starts_with("allow(") {
                continue;
            }
            let risky_suppression = directive.contains("unsafe")
                || directive.contains("warnings")
                || directive.contains("clippy::all")
                || directive.contains("clippy::pedantic");
            let stale_code_suppression =
                directive.contains("dead_code") || directive.contains("unused");
            let file_path = dv_str(&row[3]);
            if !rust_fact_allowed(requested_scope, &file_path, &from_node, &directive) {
                continue;
            }
            let scope = rust_fact_scope_label(&file_path, &from_node, &directive);
            let severity = if scope == "test" {
                "info"
            } else if risky_suppression {
                "warning"
            } else {
                "info"
            };
            let priority_score = scoped_priority(
                if risky_suppression {
                    75
                } else if stale_code_suppression {
                    50
                } else {
                    35
                },
                scope,
            );
            findings.push(json!({
                "kind": if stale_code_suppression { "lint_suppression_stale_code" } else { "lint_suppression" },
                "category": "lint_suppression",
                "severity": severity,
                "priority_score": priority_score,
                "scope": scope,
                "confidence": "high",
                "target": from_node,
                "file_path": file_path,
                "line": dv_i64(&row[4]),
                "rationale": "A lint suppression weakens compiler feedback and should be explicit, narrow, and temporary.",
                "remediation": "Remove the suppression if possible, or narrow it to the smallest item with a documented reason.",
                "evidence": {
                    "directive": directive,
                    "edge_type": edge_type,
                },
                "validation": ["Inspect the annotated item", "Run cargo clippy or cargo check without the suppression", "Rescan with rust_scan"],
            }));
        }

        for row in &calls.rows {
            let caller = dv_str(&row[0]);
            let callee = dv_str(&row[1]);
            let short = rust_path_short_name(&callee).to_string();
            let Some((kind, base_score, remediation)) = (match short.as_str() {
                "unwrap" => Some((
                    "unchecked_unwrap",
                    78,
                    "Propagate the error, handle the None/Err branch, or constrain this call to tests and fixtures.",
                )),
                "expect" => Some((
                    "unchecked_expect",
                    68,
                    "Keep expect messages for impossible states only; otherwise propagate or handle the error explicitly.",
                )),
                _ => None,
            }) else {
                continue;
            };
            let inbound = inbound_by_callee.get(&callee).map_or(0, BTreeSet::len);
            let file_path = dv_str(&row[2]);
            if !rust_fact_allowed(requested_scope, &file_path, &caller, &callee) {
                continue;
            }
            let scope = rust_fact_scope_label(&file_path, &caller, &callee);
            let raw_priority = base_score + usize_to_i64(inbound.min(10));
            let remediation = if scope == "test" {
                "Keep test unwraps when they express fixture setup or assertion intent; otherwise use expect with an intent-specific message."
            } else {
                remediation
            };
            findings.push(json!({
                "kind": kind,
                "category": "error_handling",
                "severity": if scope == "test" { "info" } else { "warning" },
                "priority_score": scoped_priority(raw_priority, scope),
                "scope": scope,
                "confidence": "medium",
                "target": format!("{caller}->{callee}"),
                "file_path": file_path,
                "line": dv_i64(&row[3]),
                "rationale": "Unchecked Option/Result handling can turn a recoverable state into a panic on an architecture path.",
                "remediation": remediation,
                "evidence": {
                    "caller": caller,
                    "callee": callee,
                    "context": dv_str(&row[4]),
                    "callee_inbound_callers": inbound,
                },
                "validation": ["Inspect the caller path", "Run cargo test", "Use rust_impact call_graph_callers if the callee is project-local"],
            }));
        }

        for row in &references.rows {
            let reference_kind = dv_str(&row[2]);
            if reference_kind != "macro" {
                continue;
            }
            let to_path = dv_str(&row[1]);
            let short = rust_path_short_name(&to_path)
                .trim_end_matches('!')
                .to_string();
            let Some((kind, severity, priority_score, remediation)) = (match short.as_str() {
                "panic" => Some((
                    "panic_macro",
                    "warning",
                    82,
                    "Return a typed error or isolate the panic at a process boundary.",
                )),
                "todo" => Some((
                    "todo_macro",
                    "warning",
                    72,
                    "Replace the placeholder with implemented behavior or track it outside production code.",
                )),
                "unimplemented" => Some((
                    "unimplemented_macro",
                    "warning",
                    76,
                    "Replace the placeholder before this path is reachable from production code.",
                )),
                "dbg" => Some((
                    "debug_macro",
                    "info",
                    44,
                    "Remove debug-only output before relying on this path in normal workflows.",
                )),
                _ => None,
            }) else {
                continue;
            };
            let file_path = dv_str(&row[0]);
            if !rust_fact_allowed(requested_scope, &file_path, &to_path, &reference_kind) {
                continue;
            }
            let scope = rust_fact_scope_label(&file_path, &to_path, &reference_kind);
            findings.push(json!({
                "kind": kind,
                "category": "control_flow_debt",
                "severity": if scope == "test" { "info" } else { severity },
                "priority_score": scoped_priority(priority_score, scope),
                "scope": scope,
                "confidence": "high",
                "target": to_path,
                "file_path": file_path,
                "line": dv_i64(&row[3]),
                "rationale": "Macro-level control flow can hide unfinished or fail-fast behavior from graph-level refactor planning.",
                "remediation": remediation,
                "evidence": {
                    "reference_kind": reference_kind,
                    "context": dv_str(&row[4]),
                },
                "validation": ["Inspect the source path", "Run cargo test", "Rescan with rust_scan"],
            }));
        }

        let mut symbol_by_alias: BTreeMap<String, Vec<PracticeSymbolAlias>> = BTreeMap::new();
        for row in &symbols.rows {
            let name = dv_str(&row[0]);
            let context = dv_str(&row[2]);
            let file_path = dv_str(&row[3]);
            if !rust_fact_allowed(requested_scope, &file_path, &name, "") {
                continue;
            }
            let start_line = dv_i64(&row[4]);
            let visibility = dv_str(&row[6]);
            for alias in symbol_lookup_aliases(&name) {
                symbol_by_alias.entry(alias).or_default().push((
                    name.clone(),
                    context.clone(),
                    file_path.clone(),
                    start_line,
                    visibility.clone(),
                ));
            }
        }
        let mut callers_by_symbol: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for row in &calls.rows {
            let caller = dv_str(&row[0]);
            let callee = dv_str(&row[1]);
            let file_path = dv_str(&row[2]);
            if !rust_fact_allowed(requested_scope, &file_path, &caller, &callee) {
                continue;
            }
            let Some(matches) = symbol_by_alias.get(&callee) else {
                continue;
            };
            for (symbol, _, _, _, _) in matches {
                callers_by_symbol
                    .entry(symbol.clone())
                    .or_default()
                    .insert(caller.clone());
            }
        }
        for row in &symbols.rows {
            let name = dv_str(&row[0]);
            let kind = dv_str(&row[1]);
            let visibility = dv_str(&row[6]);
            if visibility == "public" || !matches!(kind.as_str(), "function" | "method") {
                continue;
            }
            let Some(callers) = callers_by_symbol.get(&name) else {
                continue;
            };
            if callers.len() < 4 {
                continue;
            }
            let file_path = dv_str(&row[3]);
            if !rust_fact_allowed(requested_scope, &file_path, &name, "") {
                continue;
            }
            let scope = rust_fact_scope_label(&file_path, &name, "");
            findings.push(json!({
                "kind": "high_fan_in_private_symbol",
                "category": "coupling",
                "severity": "info",
                "priority_score": scoped_priority(42 + usize_to_i64(callers.len().min(20)), scope),
                "scope": scope,
                "confidence": "medium",
                "target": name,
                "file_path": file_path,
                "line": dv_i64(&row[4]),
                "rationale": "A private function or method has broad fan-in; changes to it may deserve a clearer boundary or extracted helper API.",
                "remediation": "Review whether the symbol should remain private implementation detail, become a deliberate facade, or be split by caller intent.",
                "evidence": {
                    "kind": kind,
                    "context": dv_str(&row[2]),
                    "visibility": visibility,
                    "caller_count": callers.len(),
                    "callers": callers.iter().cloned().collect::<Vec<_>>(),
                },
                "validation": ["Use rust_impact call_graph_callers", "Check ownership before extracting", "Run cargo test after refactor"],
            }));
        }

        findings.sort_by(|left, right| {
            right["priority_score"]
                .as_i64()
                .unwrap_or(0)
                .cmp(&left["priority_score"].as_i64().unwrap_or(0))
                .then_with(|| {
                    left["kind"]
                        .as_str()
                        .unwrap_or("")
                        .cmp(right["kind"].as_str().unwrap_or(""))
                })
                .then_with(|| {
                    left["target"]
                        .as_str()
                        .unwrap_or("")
                        .cmp(right["target"].as_str().unwrap_or(""))
                })
        });
        findings.truncate(50);

        let actionable_count = findings
            .iter()
            .filter(|finding| finding["scope"].as_str() != Some("test"))
            .count();
        let test_count = findings
            .iter()
            .filter(|finding| finding["scope"].as_str() == Some("test"))
            .count();
        let production_count = findings
            .iter()
            .filter(|finding| finding["scope"].as_str() == Some("production"))
            .count();

        Ok(json!({
            "analysis": "practice_findings",
            "scope": requested_scope.as_str(),
            "findings": findings,
            "count": findings.len(),
            "summary": {
                "returned_count": findings.len(),
                "actionable_count": actionable_count,
                "informational_count": findings.len().saturating_sub(actionable_count),
                "production_count": production_count,
                "test_count": test_count,
            },
            "fact_counts": {
                "symbols": symbols.rows.len(),
                "call_edges": calls.rows.len(),
                "ast_edges": ast_edges.rows.len(),
                "reference_edges": references.rows.len(),
            },
            "note": "Practice findings rank Rust safety and maintainability signals with graph evidence; validate with compiler, Clippy, tests, and review before editing.",
        }))
    }

    pub fn field_usage(
        &self,
        workspace: &str,
        field_type: &str,
    ) -> Result<Vec<(String, String, String, String)>> {
        let params = params_map(&[("ws", workspace), ("field_type", field_type)]);
        let rows = self
            .run_script(
                "?[ctx, owner_kind, owner, field_name] := \
                *field{workspace: $ws, context: ctx, owner_kind, owner, \
                       name: field_name, field_type: $field_type @ 'NOW'}",
                params,
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("field_usage: {:?}", e))?;
        Ok(rows
            .rows
            .iter()
            .map(|row| {
                (
                    dv_str(&row[0]),
                    dv_str(&row[1]),
                    dv_str(&row[2]),
                    dv_str(&row[3]),
                )
            })
            .collect())
    }

    pub fn method_search(
        &self,
        workspace: &str,
        method_name: &str,
    ) -> Result<Vec<(String, String, String, String)>> {
        let params = params_map(&[("ws", workspace), ("method_name", method_name)]);
        let rows = self
            .run_script(
                "?[ctx, owner_kind, owner, return_type] := \
                *method{workspace: $ws, context: ctx, owner_kind, owner, \
                        name: $method_name, return_type @ 'NOW'}",
                params,
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("method_search: {:?}", e))?;
        Ok(rows
            .rows
            .iter()
            .map(|row| {
                (
                    dv_str(&row[0]),
                    dv_str(&row[1]),
                    dv_str(&row[2]),
                    dv_str(&row[3]),
                )
            })
            .collect())
    }

    pub(crate) fn shared_fields(&self, workspace: &str) -> Result<Vec<SharedField>> {
        let params = params_map(&[("ws", workspace)]);
        let rows = self
            .run_script(
                "entity_field[ctx, owner, name, ft] := \
                *field{workspace: $ws, context: ctx, owner_kind: 'entity', \
                       owner, name, field_type: ft @ 'NOW'} \
             event_field[ctx, owner, name, ft] := \
                *field{workspace: $ws, context: ctx, owner_kind: 'event', \
                       owner, name, field_type: ft @ 'NOW'} \
             ?[ctx, entity, event, field_name, field_type] := \
                entity_field[ctx, entity, field_name, field_type], \
                event_field[ctx, event, field_name, field_type]",
                params,
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("shared_fields: {:?}", e))?;
        Ok(rows
            .rows
            .iter()
            .map(|row| {
                (
                    dv_str(&row[0]),
                    dv_str(&row[1]),
                    dv_str(&row[2]),
                    dv_str(&row[3]),
                    dv_str(&row[4]),
                )
            })
            .collect())
    }

    pub fn dependency_graph(&self, workspace: &str) -> Result<serde_json::Value> {
        let params = params_map(&[("ws", workspace)]);
        let contexts = self
            .run_script(
                "?[name, module_path] := *context{workspace: $ws, name, module_path @ 'NOW'}",
                params.clone(),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("dependency_graph contexts: {:?}", e))?;
        let deps = self
            .run_script(
                "?[from_ctx, to_ctx] := *context_dep{workspace: $ws, from_ctx, to_ctx @ 'NOW'}",
                params.clone(),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("dependency_graph deps: {:?}", e))?;
        let circular = self.circular_deps(workspace)?;

        Ok(json!({
            "nodes": contexts.rows.iter()
                .map(|r| json!({"name": dv_str(&r[0]), "module_path": dv_str(&r[1])}))
                .collect::<Vec<_>>(),
            "edges": deps.rows.iter()
                .map(|r| json!({"from": dv_str(&r[0]), "to": dv_str(&r[1])}))
                .collect::<Vec<_>>(),
            "circular_dependencies": circular.iter()
                .map(|(a, b)| json!({"a": a, "b": b}))
                .collect::<Vec<_>>(),
        }))
    }

    // ── Full-Text Search ──────────────────────────────────────────────────

    /// Search architecture entities by keyword using CozoDB FTS indices.
    /// Returns matches across contexts, entities, services, events, and decisions.
    pub fn search_text(
        &self,
        workspace: &str,
        query: &str,
        limit: usize,
    ) -> Result<serde_json::Value> {
        let ws = canonicalize_path(workspace);
        let mut params = params_map(&[("ws", &ws), ("q", query)]);
        params.insert("k".into(), int_dv(usize_to_i64(limit)));

        let mut results: Vec<serde_json::Value> = Vec::new();

        // Search contexts
        if let Ok(r) = self.run_script(
            "?[name, description, score] := ~context:fts{workspace, name | query: $q, k: $k, bind_score: score}, \
             workspace = $ws, *context{workspace, name, description @ 'NOW'}",
            params.clone(), ScriptMutability::Immutable,
        ) {
            for row in &r.rows {
                results.push(json!({"kind": "context", "name": dv_str(&row[0]), "description": dv_str(&row[1]), "score": dv_str(&row[2])}));
            }
        }

        // Search entities
        if let Ok(r) = self.run_script(
            "?[context, name, description, score] := ~entity:fts{workspace, context, name | query: $q, k: $k, bind_score: score}, \
             workspace = $ws, *entity{workspace, context, name, description @ 'NOW'}",
            params.clone(), ScriptMutability::Immutable,
        ) {
            for row in &r.rows {
                results.push(json!({"kind": "entity", "context": dv_str(&row[0]), "name": dv_str(&row[1]), "description": dv_str(&row[2]), "score": dv_str(&row[3])}));
            }
        }

        // Search services
        if let Ok(r) = self.run_script(
            "?[context, name, description, score] := ~service:fts{workspace, context, name | query: $q, k: $k, bind_score: score}, \
             workspace = $ws, *service{workspace, context, name, description @ 'NOW'}",
            params.clone(), ScriptMutability::Immutable,
        ) {
            for row in &r.rows {
                results.push(json!({"kind": "service", "context": dv_str(&row[0]), "name": dv_str(&row[1]), "description": dv_str(&row[2]), "score": dv_str(&row[3])}));
            }
        }

        // Search events
        if let Ok(r) = self.run_script(
            "?[context, name, description, score] := ~event:fts{workspace, context, name | query: $q, k: $k, bind_score: score}, \
             workspace = $ws, *event{workspace, context, name, description @ 'NOW'}",
            params.clone(), ScriptMutability::Immutable,
        ) {
            for row in &r.rows {
                results.push(json!({"kind": "event", "context": dv_str(&row[0]), "name": dv_str(&row[1]), "description": dv_str(&row[2]), "score": dv_str(&row[3])}));
            }
        }

        // Search decision titles
        if let Ok(r) = self.run_script(
            "?[id, title, score] := ~architectural_decision:title_fts{workspace, id | query: $q, k: $k, bind_score: score}, \
             workspace = $ws, *architectural_decision{workspace, id, title @ 'NOW'}",
            params.clone(), ScriptMutability::Immutable,
        ) {
            for row in &r.rows {
                results.push(json!({"kind": "architectural_decision", "id": dv_str(&row[0]), "title": dv_str(&row[1]), "score": dv_str(&row[2])}));
            }
        }

        // Search decision rationales
        if let Ok(r) = self.run_script(
            "?[id, title, rationale, score] := ~architectural_decision:rationale_fts{workspace, id | query: $q, k: $k, bind_score: score}, \
             workspace = $ws, *architectural_decision{workspace, id, title, rationale @ 'NOW'}",
            params.clone(), ScriptMutability::Immutable,
        ) {
            for row in &r.rows {
                // Avoid duplicate if already found by title
                let id = dv_str(&row[0]);
                if !results.iter().any(|r| r["kind"] == "architectural_decision" && r["id"] == id) {
                    results.push(json!({"kind": "architectural_decision", "id": id, "title": dv_str(&row[1]), "rationale_match": dv_str(&row[2]), "score": dv_str(&row[3])}));
                }
            }
        }

        // Search invariant text
        if let Ok(r) = self.run_script(
            "?[context, entity, text, score] := ~invariant:text_fts{workspace, context, entity, idx | query: $q, k: $k, bind_score: score}, \
             workspace = $ws, *invariant{workspace, context, entity, idx, text @ 'NOW'}",
            params.clone(), ScriptMutability::Immutable,
        ) {
            for row in &r.rows {
                results.push(json!({"kind": "invariant", "context": dv_str(&row[0]), "entity": dv_str(&row[1]), "text": dv_str(&row[2]), "score": dv_str(&row[3])}));
            }
        }

        if results.is_empty() && !query.trim().is_empty() {
            let needle = query.to_lowercase();
            if let Some(model) = self.load_actual(&ws)? {
                for context in &model.bounded_contexts {
                    if text_matches(&context.name, &needle)
                        || text_matches(&context.description, &needle)
                    {
                        results.push(json!({
                            "kind": "context",
                            "name": &context.name,
                            "description": &context.description,
                            "score": "1.0",
                            "search_mode": "model_scan",
                        }));
                    }
                    for entity in &context.entities {
                        if text_matches(&entity.name, &needle)
                            || text_matches(&entity.description, &needle)
                        {
                            results.push(json!({
                                "kind": "entity",
                                "context": &context.name,
                                "name": &entity.name,
                                "description": &entity.description,
                                "score": "1.0",
                                "search_mode": "model_scan",
                            }));
                        }
                        for invariant in &entity.invariants {
                            if text_matches(invariant, &needle) {
                                results.push(json!({
                                    "kind": "invariant",
                                    "context": &context.name,
                                    "entity": &entity.name,
                                    "text": invariant,
                                    "score": "1.0",
                                    "search_mode": "model_scan",
                                }));
                            }
                        }
                    }
                    for service in &context.services {
                        if text_matches(&service.name, &needle)
                            || text_matches(&service.description, &needle)
                        {
                            results.push(json!({
                                "kind": "service",
                                "context": &context.name,
                                "name": &service.name,
                                "description": &service.description,
                                "score": "1.0",
                                "search_mode": "model_scan",
                            }));
                        }
                    }
                    for event in &context.events {
                        if text_matches(&event.name, &needle)
                            || text_matches(&event.description, &needle)
                        {
                            results.push(json!({
                                "kind": "event",
                                "context": &context.name,
                                "name": &event.name,
                                "description": &event.description,
                                "score": "1.0",
                                "search_mode": "model_scan",
                            }));
                        }
                    }
                }
                for decision in &model.architectural_decisions {
                    if text_matches(&decision.id, &needle)
                        || text_matches(&decision.title, &needle)
                        || text_matches(&decision.rationale, &needle)
                    {
                        results.push(json!({
                            "kind": "architectural_decision",
                            "id": &decision.id,
                            "title": &decision.title,
                            "rationale_match": &decision.rationale,
                            "score": "1.0",
                            "search_mode": "model_scan",
                        }));
                    }
                }
                for source_file in &model.source_files {
                    if text_matches(&source_file.path, &needle)
                        || text_matches(&source_file.context, &needle)
                        || text_matches(&source_file.language, &needle)
                    {
                        results.push(json!({
                            "kind": "source_file",
                            "path": &source_file.path,
                            "context": &source_file.context,
                            "language": &source_file.language,
                            "score": "1.0",
                            "search_mode": "rust_fact_scan",
                        }));
                    }
                }
                for symbol in &model.symbols {
                    if text_matches(&symbol.name, &needle)
                        || text_matches(&symbol.kind, &needle)
                        || text_matches(&symbol.context, &needle)
                        || text_matches(&symbol.file_path, &needle)
                    {
                        results.push(json!({
                            "kind": "symbol",
                            "name": &symbol.name,
                            "symbol_kind": &symbol.kind,
                            "context": &symbol.context,
                            "file": &symbol.file_path,
                            "start_line": symbol.start_line,
                            "end_line": symbol.end_line,
                            "visibility": &symbol.visibility,
                            "score": "1.0",
                            "search_mode": "rust_fact_scan",
                        }));
                    }
                }
                for import in &model.import_edges {
                    if text_matches(&import.from_file, &needle)
                        || text_matches(&import.to_module, &needle)
                        || text_matches(&import.context, &needle)
                    {
                        results.push(json!({
                            "kind": "import_edge",
                            "from_file": &import.from_file,
                            "to_module": &import.to_module,
                            "context": &import.context,
                            "score": "1.0",
                            "search_mode": "rust_fact_scan",
                        }));
                    }
                }
                for reference in &model.reference_edges {
                    if text_matches(&reference.from_file, &needle)
                        || text_matches(&reference.to_path, &needle)
                        || text_matches(&reference.reference_kind, &needle)
                        || text_matches(&reference.context, &needle)
                    {
                        results.push(json!({
                            "kind": "reference_edge",
                            "from_file": &reference.from_file,
                            "to_path": &reference.to_path,
                            "reference_kind": &reference.reference_kind,
                            "line": reference.line,
                            "context": &reference.context,
                            "score": "1.0",
                            "search_mode": "rust_fact_scan",
                        }));
                    }
                }
                for call in &model.call_edges {
                    if text_matches(&call.caller, &needle)
                        || text_matches(&call.callee, &needle)
                        || text_matches(&call.file_path, &needle)
                        || text_matches(&call.context, &needle)
                    {
                        results.push(json!({
                            "kind": "calls_symbol",
                            "caller": &call.caller,
                            "callee": &call.callee,
                            "file": &call.file_path,
                            "line": call.line,
                            "context": &call.context,
                            "score": "1.0",
                            "search_mode": "rust_fact_scan",
                        }));
                    }
                }
            }
            for (context, layer) in self.list_layer_assignments(&ws)? {
                let searchable =
                    format!("policy architecture layer assignment context {context} layer {layer}");
                if text_matches(&searchable, &needle) {
                    results.push(json!({
                        "kind": "layer_assignment",
                        "context": context,
                        "layer": layer,
                        "score": "1.0",
                        "search_mode": "policy_scan",
                    }));
                }
            }
            for (constraint_kind, source, target, rule) in self.list_dependency_constraints(&ws)? {
                let searchable = format!(
                    "policy architecture dependency constraint {constraint_kind} {source} {target} {rule}"
                );
                if text_matches(&searchable, &needle) {
                    results.push(json!({
                        "kind": "dependency_constraint",
                        "constraint_kind": constraint_kind,
                        "source": source,
                        "target": target,
                        "rule": rule,
                        "score": "1.0",
                        "search_mode": "policy_scan",
                    }));
                }
            }
        }

        results.sort_by(|a, b| {
            let sa: f64 = a["score"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0);
            let sb: f64 = b["score"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0);
            sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
        });
        let total_before_limit = results.len();
        let truncated = total_before_limit > limit;
        results.truncate(limit);

        Ok(json!({
            "query": query,
            "total_before_limit": total_before_limit,
            "truncated": truncated,
            "results": results,
            "count": results.len(),
        }))
    }

    // ── Graph Algorithms (CozoDB Fixed Rules) ─────────────────────────────

    /// Compute PageRank over the context dependency graph.
    /// Fetch the context dependency graph as (nodes, directed edges). Nodes
    /// include every named context plus any endpoint that appears only in an
    /// edge, sorted for deterministic output.
    fn context_graph(&self, workspace: &str) -> Result<ContextGraph> {
        let params = params_map(&[("ws", workspace)]);
        let contexts = self
            .run_script(
                "?[ctx] := *context{workspace: $ws, name: ctx @ 'NOW'}",
                params.clone(),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("context_graph contexts: {:?}", e))?;
        let edges = self
            .run_script(
                "?[from_ctx, to_ctx] := *context_dep{workspace: $ws, from_ctx, to_ctx @ 'NOW'}",
                params,
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("context_graph edges: {:?}", e))?;
        let edge_list: Vec<(String, String)> = edges
            .rows
            .iter()
            .map(|r| (dv_str(&r[0]), dv_str(&r[1])))
            .collect();
        let mut node_set: BTreeSet<String> = contexts.rows.iter().map(|r| dv_str(&r[0])).collect();
        for (from, to) in &edge_list {
            node_set.insert(from.clone());
            node_set.insert(to.clone());
        }
        Ok((node_set.into_iter().collect(), edge_list))
    }

    /// PageRank over the context dependency graph (power iteration, damping 0.85).
    /// Computed in pure Rust — the graph is tiny (one node per bounded context) —
    /// so it works regardless of which Cozo fixed-rules the build ships.
    pub fn pagerank(&self, workspace: &str) -> Result<Vec<(String, f64)>> {
        let (nodes, edges) = self.context_graph(workspace)?;
        let n = nodes.len();
        if n == 0 {
            return Ok(Vec::new());
        }
        let idx: HashMap<&str, usize> = nodes
            .iter()
            .enumerate()
            .map(|(i, s)| (s.as_str(), i))
            .collect();
        let mut out: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (from, to) in &edges {
            if let (Some(&fi), Some(&ti)) = (idx.get(from.as_str()), idx.get(to.as_str())) {
                out[fi].push(ti);
            }
        }
        let damping = 0.85;
        let n_f64 = usize_to_f64(n);
        let base = (1.0 - damping) / n_f64;
        let mut rank = vec![1.0_f64 / n_f64; n];
        for _ in 0..200 {
            let mut next = vec![base; n];
            let mut dangling = 0.0;
            for i in 0..n {
                if out[i].is_empty() {
                    dangling += damping * rank[i] / n_f64;
                } else {
                    let share = damping * rank[i] / usize_to_f64(out[i].len());
                    for &j in &out[i] {
                        next[j] += share;
                    }
                }
            }
            for v in next.iter_mut() {
                *v += dangling;
            }
            let diff: f64 = next.iter().zip(&rank).map(|(a, b)| (a - b).abs()).sum();
            rank = next;
            if diff < 1e-9 {
                break;
            }
        }
        let mut ranked: Vec<(String, f64)> = nodes.into_iter().zip(rank).collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        Ok(ranked)
    }

    /// Community detection over the context dependency graph via synchronous
    /// label propagation on the undirected projection. Pure Rust; deterministic
    /// (ties broken by lowest label). Community ids are normalized to 0..k.
    pub fn community_detection(&self, workspace: &str) -> Result<Vec<(String, u64)>> {
        let (nodes, edges) = self.context_graph(workspace)?;
        let n = nodes.len();
        if n == 0 {
            return Ok(Vec::new());
        }
        let idx: HashMap<&str, usize> = nodes
            .iter()
            .enumerate()
            .map(|(i, s)| (s.as_str(), i))
            .collect();
        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (from, to) in &edges {
            let (Some(&fi), Some(&ti)) = (idx.get(from.as_str()), idx.get(to.as_str())) else {
                continue;
            };
            if fi != ti {
                adj[fi].push(ti);
                adj[ti].push(fi);
            }
        }
        let mut label: Vec<usize> = (0..n).collect();
        for _ in 0..100 {
            let mut changed = false;
            for v in 0..n {
                if adj[v].is_empty() {
                    continue;
                }
                let mut counts: BTreeMap<usize, usize> = BTreeMap::new();
                for &w in &adj[v] {
                    *counts.entry(label[w]).or_default() += 1;
                }
                // Highest neighbor-label count; ties broken toward the lowest label.
                let best = counts
                    .iter()
                    .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
                    .map(|(l, _)| *l)
                    .unwrap_or(label[v]);
                if best != label[v] {
                    label[v] = best;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let mut remap: BTreeMap<usize, u64> = BTreeMap::new();
        let mut next_id = 0_u64;
        Ok(nodes
            .into_iter()
            .enumerate()
            .map(|(i, name)| {
                let cid = *remap.entry(label[i]).or_insert_with(|| {
                    let id = next_id;
                    next_id += 1;
                    id
                });
                (name, cid)
            })
            .collect())
    }

    /// Betweenness centrality over the context dependency graph (Brandes'
    /// algorithm, directed unweighted). Pure Rust.
    pub fn betweenness_centrality(&self, workspace: &str) -> Result<Vec<(String, f64)>> {
        let (nodes, edges) = self.context_graph(workspace)?;
        let n = nodes.len();
        if n == 0 {
            return Ok(Vec::new());
        }
        let idx: HashMap<&str, usize> = nodes
            .iter()
            .enumerate()
            .map(|(i, s)| (s.as_str(), i))
            .collect();
        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (from, to) in &edges {
            if let (Some(&fi), Some(&ti)) = (idx.get(from.as_str()), idx.get(to.as_str())) {
                adj[fi].push(ti);
            }
        }
        let mut bc = vec![0.0_f64; n];
        for s in 0..n {
            let mut stack: Vec<usize> = Vec::new();
            let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
            let mut sigma = vec![0.0_f64; n];
            sigma[s] = 1.0;
            let mut dist = vec![-1_i64; n];
            dist[s] = 0;
            let mut queue: VecDeque<usize> = VecDeque::new();
            queue.push_back(s);
            while let Some(v) = queue.pop_front() {
                stack.push(v);
                for &w in &adj[v] {
                    if dist[w] < 0 {
                        dist[w] = dist[v] + 1;
                        queue.push_back(w);
                    }
                    if dist[w] == dist[v] + 1 {
                        sigma[w] += sigma[v];
                        preds[w].push(v);
                    }
                }
            }
            let mut delta = vec![0.0_f64; n];
            while let Some(w) = stack.pop() {
                for &v in &preds[w] {
                    delta[v] += (sigma[v] / sigma[w]) * (1.0 + delta[w]);
                }
                if w != s {
                    bc[w] += delta[w];
                }
            }
        }
        let mut ranked: Vec<(String, f64)> = nodes.into_iter().zip(bc).collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        Ok(ranked)
    }

    /// Compute in-degree and out-degree for each context in the dependency graph.
    pub fn degree_centrality(&self, workspace: &str) -> Result<Vec<(String, u32, u32)>> {
        let params = params_map(&[("ws", workspace)]);
        let contexts = self
            .run_script(
                "?[ctx] := *context{workspace: $ws, name: ctx @ 'NOW'}",
                params.clone(),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("degree_centrality contexts: {:?}", e))?;
        let edges = self
            .run_script(
                "?[from_ctx, to_ctx] := *context_dep{workspace: $ws, from_ctx, to_ctx @ 'NOW'}",
                params,
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("degree_centrality edges: {:?}", e))?;

        let mut degrees: BTreeMap<String, (u32, u32)> = BTreeMap::new();
        for row in &contexts.rows {
            degrees.entry(dv_str(&row[0])).or_insert((0, 0));
        }
        for row in &edges.rows {
            let from_ctx = dv_str(&row[0]);
            let to_ctx = dv_str(&row[1]);
            degrees.entry(from_ctx.clone()).or_insert((0, 0)).1 += 1;
            degrees.entry(to_ctx).or_insert((0, 0)).0 += 1;
        }

        Ok(degrees
            .into_iter()
            .map(|(ctx, (in_d, out_d))| (ctx, in_d, out_d))
            .collect())
    }

    /// Compute topological ordering of context dependencies (if acyclic).
    pub fn topological_order(&self, workspace: &str) -> Result<serde_json::Value> {
        let params = params_map(&[("ws", workspace)]);
        let contexts = self
            .run_script(
                "?[ctx] := *context{workspace: $ws, name: ctx @ 'NOW'}",
                params.clone(),
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("topological_order contexts: {:?}", e))?;
        let edges = self
            .run_script(
                "?[from_ctx, to_ctx] := *context_dep{workspace: $ws, from_ctx, to_ctx @ 'NOW'}",
                params,
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("topological_order edges: {:?}", e))?;

        let mut dependents: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut indegree: BTreeMap<String, u32> = BTreeMap::new();
        for row in &contexts.rows {
            indegree.entry(dv_str(&row[0])).or_insert(0);
        }
        for row in &edges.rows {
            let from_ctx = dv_str(&row[0]);
            let to_ctx = dv_str(&row[1]);
            dependents
                .entry(to_ctx.clone())
                .or_default()
                .insert(from_ctx.clone());
            *indegree.entry(from_ctx).or_insert(0) += 1;
            indegree.entry(to_ctx).or_insert(0);
        }

        let mut ready: BTreeSet<String> = indegree
            .iter()
            .filter(|(_, count)| **count == 0)
            .map(|(ctx, _)| ctx.clone())
            .collect();
        let mut ordered = Vec::new();

        while let Some(ctx) = ready.iter().next().cloned() {
            ready.remove(&ctx);
            let order = usize_to_i64(ordered.len());
            ordered.push((ctx.clone(), order));
            if let Some(context_dependents) = dependents.get(&ctx) {
                for dependent in context_dependents {
                    if let Some(count) = indegree.get_mut(dependent) {
                        *count = count.saturating_sub(1);
                        if *count == 0 {
                            ready.insert(dependent.clone());
                        }
                    }
                }
            }
        }

        if ordered.len() == indegree.len() {
            Ok(json!({
                "status": "acyclic",
                "order": ordered.iter().map(|(n, o)| json!({"context": n, "order": o})).collect::<Vec<_>>(),
            }))
        } else {
            let cycles = self.circular_deps(workspace)?;
            let ordered_contexts: BTreeSet<_> =
                ordered.iter().map(|(ctx, _)| ctx.clone()).collect();
            let remaining = indegree
                .keys()
                .filter(|ctx| !ordered_contexts.contains(*ctx))
                .cloned()
                .collect::<Vec<_>>();
            Ok(json!({
                "status": "cyclic",
                "message": "Graph contains cycles; topological sort is not possible.",
                "cycles": cycles.iter().map(|(a, b)| json!({"from": a, "to": b})).collect::<Vec<_>>(),
                "remaining_contexts": remaining,
            }))
        }
    }

    // ── Metalayer: Model Health ────────────────────────────────────────────

    pub fn model_health(&self, workspace: &str) -> Result<ModelHealth> {
        let canonical = canonicalize_path(workspace);
        self.refresh_runtime_constraints(&canonical)?;
        let circular = self.circular_deps(&canonical)?;
        let module_cycles = self.module_cycles(&canonical).unwrap_or_default();
        let violations = self.layer_violations(&canonical)?;
        let missing_invariants = self.aggregate_roots_without_invariants(&canonical)?;
        let orphans = self.orphan_contexts(&canonical)?;
        let complexity = self.context_complexity(&canonical)?;
        let policy_coverage = self.policy_coverage(&canonical, &complexity)?;
        let policy_result = self.evaluate_policy_violations_canonical(&canonical)?;
        let policy_violations = policy_result
            .get("violations")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        let god_contexts: Vec<String> = complexity
            .iter()
            .filter(|c| c.entity_count + c.service_count > 10)
            .map(|c| c.context.clone())
            .collect();
        let unsourced_events = self.unsourced_events(&canonical)?;

        // Graph algorithms via CozoDB fixed rules
        let bottleneck_contexts: Vec<String> = match self.betweenness_centrality(&canonical) {
            Ok(rows) => rows
                .into_iter()
                .filter(|(_, c)| *c > 0.0)
                .map(|(name, _)| name)
                .collect(),
            Err(e) => {
                tracing::debug!("Betweenness centrality unavailable for model_health: {e}");
                Vec::new()
            }
        };
        let communities = match self.community_detection(&canonical) {
            Ok(rows) => rows,
            Err(e) => {
                tracing::debug!("Community detection unavailable for model_health: {e}");
                Vec::new()
            }
        };

        let critical = circular.len() + violations.len() + policy_violations.len();
        let warnings = missing_invariants.len() + god_contexts.len() + unsourced_events.len();
        let policy_gaps = if policy_coverage.context_count == 0 {
            0
        } else {
            policy_coverage.missing_layer_assignments.len()
                + usize::from(policy_coverage.dependency_constraint_count == 0)
        };
        let info = orphans.len() + policy_gaps;
        let score = 100_i64
            .saturating_sub(usize_to_i64(critical).saturating_mul(20))
            .saturating_sub(usize_to_i64(warnings).saturating_mul(5))
            .saturating_sub(usize_to_i64(info).saturating_mul(2));
        let score = u32::try_from(score.max(0)).unwrap_or(0);

        Ok(ModelHealth {
            score,
            circular_deps: circular.into_iter().map(|(a, b)| [a, b]).collect(),
            module_cycles,
            layer_violations: violations
                .into_iter()
                .map(|(ctx, svc, dep)| LayerViolation {
                    context: ctx,
                    domain_service: svc,
                    infra_dependency: dep,
                })
                .collect(),
            missing_invariants: missing_invariants
                .into_iter()
                .map(|(ctx, ent)| [ctx, ent])
                .collect(),
            orphan_contexts: orphans,
            god_contexts,
            unsourced_events,
            complexity,
            policy_coverage,
            policy_violations,
            bottleneck_contexts,
            communities: communities
                .into_iter()
                .map(|(name, cid)| CommunityMembership {
                    context: name,
                    community: cid,
                })
                .collect(),
        })
    }

    fn orphan_contexts(&self, workspace: &str) -> Result<Vec<String>> {
        let params = params_map(&[("ws", workspace)]);
        let result = self
            .run_script(
                "has_dep[ctx] := *context_dep{workspace: $ws, from_ctx: ctx, state: 'actual' @ 'NOW'} \
                 has_dep[ctx] := *context_dep{workspace: $ws, to_ctx: ctx, state: 'actual' @ 'NOW'} \
                 ?[name] := *context{workspace: $ws, name, state: 'actual' @ 'NOW'}, not has_dep[name]",
                params,
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("orphan_contexts: {:?}", e))?;
        Ok(result.rows.iter().map(|r| dv_str(&r[0])).collect())
    }

    fn context_complexity(&self, workspace: &str) -> Result<Vec<ContextComplexity>> {
        let params = params_map(&[("ws", workspace)]);
        let contexts = self
            .run_script(
                "?[ctx] := *context{workspace: $ws, name: ctx, state: 'actual' @ 'NOW'}",
                params,
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("context_complexity contexts: {:?}", e))?;

        let mut complexity = Vec::with_capacity(contexts.rows.len());
        for row in contexts.rows {
            let context = dv_str(&row[0]);
            let count_params = params_map(&[("ws", workspace), ("ctx", &context)]);
            let entity_count = self
                .run_script(
                    "?[name] := *entity{workspace: $ws, context: $ctx, name, state: 'actual' @ 'NOW'}",
                    count_params.clone(),
                    ScriptMutability::Immutable,
                )
                .map_err(|e| anyhow::anyhow!("context_complexity entity count: {:?}", e))?
                .rows
                .len();
            let entity_count = usize_to_u32(entity_count);
            let service_count = self
                .run_script(
                    "?[name] := *service{workspace: $ws, context: $ctx, name, state: 'actual' @ 'NOW'}",
                    count_params.clone(),
                    ScriptMutability::Immutable,
                )
                .map_err(|e| anyhow::anyhow!("context_complexity service count: {:?}", e))?
                .rows
                .len();
            let service_count = usize_to_u32(service_count);
            let event_count = self
                .run_script(
                    "?[name] := *event{workspace: $ws, context: $ctx, name, state: 'actual' @ 'NOW'}",
                    count_params.clone(),
                    ScriptMutability::Immutable,
                )
                .map_err(|e| anyhow::anyhow!("context_complexity event count: {:?}", e))?
                .rows
                .len();
            let event_count = usize_to_u32(event_count);
            let dep_count = self
                .run_script(
                    "?[dep] := *context_dep{workspace: $ws, from_ctx: $ctx, to_ctx: dep, state: 'actual' @ 'NOW'}",
                    count_params,
                    ScriptMutability::Immutable,
                )
                .map_err(|e| anyhow::anyhow!("context_complexity dependency count: {:?}", e))?
                .rows
                .len();
            let dep_count = usize_to_u32(dep_count);
            complexity.push(ContextComplexity {
                context,
                entity_count,
                service_count,
                event_count,
                dep_count,
            });
        }

        Ok(complexity)
    }

    fn policy_coverage(
        &self,
        workspace: &str,
        complexity: &[ContextComplexity],
    ) -> Result<PolicyCoverage> {
        let layer_assignments = self.list_layer_assignments(workspace)?;
        let dependency_constraints = self.list_dependency_constraints(workspace)?;
        let assigned_contexts = layer_assignments
            .iter()
            .map(|(context, _)| context.as_str())
            .collect::<BTreeSet<_>>();
        let missing_layer_assignments = complexity
            .iter()
            .filter(|context| !assigned_contexts.contains(context.context.as_str()))
            .map(|context| context.context.clone())
            .collect::<Vec<_>>();

        Ok(PolicyCoverage {
            context_count: complexity.len(),
            layer_assignment_count: layer_assignments.len(),
            dependency_constraint_count: dependency_constraints.len(),
            missing_layer_assignments,
        })
    }

    fn unsourced_events(&self, workspace: &str) -> Result<Vec<[String; 2]>> {
        let params = params_map(&[("ws", workspace)]);
        let result = self
            .run_script(
                "?[context, name] := *event{workspace: $ws, context, name, source: '', state: 'actual' @ 'NOW'}",
                params,
                ScriptMutability::Immutable,
            )
            .map_err(|e| anyhow::anyhow!("unsourced_events: {:?}", e))?;
        Ok(result
            .rows
            .iter()
            .map(|r| [dv_str(&r[0]), dv_str(&r[1])])
            .collect())
    }
}

// ── Helper Functions ───────────────────────────────────────────────────────

fn summarize_fact_snapshot(
    knowledge_kind: &str,
    state: &str,
    snapshot_timestamp_us: Option<i64>,
    model: Option<&DomainModel>,
) -> FactSnapshotSummary {
    let Some(model) = model else {
        return FactSnapshotSummary {
            knowledge_kind: knowledge_kind.to_string(),
            state: state.to_string(),
            available: false,
            snapshot_timestamp_us,
            context_count: 0,
            entity_count: 0,
            value_object_count: 0,
            service_count: 0,
            repository_count: 0,
            event_count: 0,
        };
    };

    FactSnapshotSummary {
        knowledge_kind: knowledge_kind.to_string(),
        state: state.to_string(),
        available: true,
        snapshot_timestamp_us,
        context_count: model.bounded_contexts.len(),
        entity_count: model
            .bounded_contexts
            .iter()
            .map(|context| context.entities.len())
            .sum(),
        value_object_count: model
            .bounded_contexts
            .iter()
            .map(|context| context.value_objects.len())
            .sum(),
        service_count: model
            .bounded_contexts
            .iter()
            .map(|context| context.services.len())
            .sum(),
        repository_count: model
            .bounded_contexts
            .iter()
            .map(|context| context.repositories.len())
            .sum(),
        event_count: model
            .bounded_contexts
            .iter()
            .map(|context| context.events.len())
            .sum(),
    }
}

/// Normalize workspace path for consistent keying.
pub fn canonicalize_path(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    let normalized = if trimmed.is_empty() && path.starts_with('/') {
        "/"
    } else {
        trimmed
    };
    match std::fs::canonicalize(normalized) {
        Ok(p) => p.to_string_lossy().to_string(),
        Err(_) => normalized.to_string(),
    }
}

/// Default Clean-Architecture layer dependency constraints, seeded into every
/// store at open.
///
/// These encode the universal "dependencies point inward" rule shared by
/// Clean / Hexagonal / Onion architecture: `domain` is innermost (may depend on
/// nothing), `application` sits above it, and `infrastructure`/presentation are
/// the outer ring. Every outward-pointing layer dependency is forbidden. Because
/// this grammar is the same for every Rust/DDD project, it ships as a default so
/// a conventionally-structured workspace gets architecture governance with no
/// hand-written policy. These rows are never written into the scanned
/// repository; explicit overrides are runtime-only for the active store session.
pub fn default_layer_constraints()
-> &'static [(&'static str, &'static str, &'static str, &'static str)] {
    &[
        ("layer", "domain", "application", "forbidden"),
        ("layer", "domain", "infrastructure", "forbidden"),
        ("layer", "domain", "presentation", "forbidden"),
        ("layer", "application", "infrastructure", "forbidden"),
        ("layer", "application", "presentation", "forbidden"),
    ]
}

fn params_map(pairs: &[(&str, &str)]) -> BTreeMap<String, cozo::DataValue> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), cozo::DataValue::Str(v.to_string().into())))
        .collect()
}

fn int_dv(n: i64) -> cozo::DataValue {
    cozo::DataValue::Num(cozo::Num::Int(n))
}

fn usize_to_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn usize_to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn u64_to_usize_saturating(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

fn i64_to_usize_saturating(value: i64) -> usize {
    if value <= 0 {
        0
    } else {
        usize::try_from(value).unwrap_or(usize::MAX)
    }
}

fn u128_to_i64_saturating(value: u128) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn usize_to_f64(value: usize) -> f64 {
    f64::from(usize_to_u32(value))
}

fn text_matches(haystack: &str, needle_lowercase: &str) -> bool {
    let haystack = haystack.to_lowercase();
    if haystack.contains(needle_lowercase) {
        return true;
    }
    // For path- or namespace-qualified needles (containing '/', '::', or '.'),
    // match only on a path-boundary suffix in either direction: an absolute-path
    // filter (`/repo/src/dht.rs`) matches a stored relative path (`src/dht.rs`)
    // and vice-versa, while a shared mid-segment like "src" or "domain" still
    // does NOT match (so `src/domain/model.rs` won't match `src/domain/analyze.rs`).
    // The loose token fallback below stays for single-token needles (symbol search).
    if needle_lowercase.contains('/')
        || needle_lowercase.contains("::")
        || needle_lowercase.contains('.')
    {
        return path_boundary_suffix(&haystack, needle_lowercase)
            || path_boundary_suffix(needle_lowercase, &haystack);
    }
    needle_lowercase
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|token| token.len() > 2)
        .any(|token| haystack.contains(token))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RustFactScope {
    Production,
    Test,
    All,
}

impl RustFactScope {
    fn parse(value: &str, default: Self) -> Result<Self> {
        match value {
            "" => Ok(default),
            "production" => Ok(Self::Production),
            "test" => Ok(Self::Test),
            "all" => Ok(Self::All),
            other => anyhow::bail!("invalid scope '{other}'; expected production, test, or all"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Production => "production",
            Self::Test => "test",
            Self::All => "all",
        }
    }

    fn allows(self, item_scope: Self) -> bool {
        matches!(self, Self::All) || self == item_scope
    }
}

fn rust_fact_scope(file_path: &str, symbol: &str, marker: &str) -> RustFactScope {
    if is_test_fact(file_path, symbol, marker) {
        RustFactScope::Test
    } else {
        RustFactScope::Production
    }
}

fn rust_fact_scope_label(file_path: &str, symbol: &str, marker: &str) -> &'static str {
    rust_fact_scope(file_path, symbol, marker).as_str()
}

fn rust_fact_allowed(
    requested: RustFactScope,
    file_path: &str,
    symbol: &str,
    marker: &str,
) -> bool {
    requested.allows(rust_fact_scope(file_path, symbol, marker))
}

/// True when both names denote the same bounded context, i.e. the edge is a self-loop.
///
/// This is a *presentation* predicate used to suppress self-edges in graph views
/// and edge counts; the underlying `context_dep` facts always retain self-loops so
/// cycle detection can still observe them. Comparison is case-sensitive: `Billing`
/// and `billing` are distinct contexts, not a self-loop.
fn same_context_name(left: &str, right: &str) -> bool {
    let left = left.trim();
    let right = right.trim();
    !left.is_empty() && left == right
}

fn is_test_fact(file_path: &str, symbol: &str, marker: &str) -> bool {
    let path = file_path.replace('\\', "/");
    let symbol = symbol.trim();
    let marker = marker.trim();
    path.ends_with("_tests.rs")
        || path.ends_with("_test.rs")
        || path.starts_with("tests/")
        || path.contains("/tests/")
        || symbol.starts_with("test_")
        || symbol.contains("::tests::")
        || marker == "test"
        || cfg_marker_contains_test(marker)
        || marker.starts_with("tokio::test")
        || marker.starts_with("async_std::test")
}

fn cfg_marker_contains_test(marker: &str) -> bool {
    marker.trim().starts_with("cfg(")
        && marker
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .any(|token| token == "test")
}

/// True if `short` equals `long`, or `short` is a suffix of `long` at a
/// path/namespace boundary (the character before the suffix is `/` or `:`).
/// Lets absolute and relative paths match without matching a shared mid-segment.
fn path_boundary_suffix(long: &str, short: &str) -> bool {
    match long.strip_suffix(short) {
        Some("") => true,
        Some(prefix) => prefix.ends_with('/') || prefix.ends_with(':'),
        None => false,
    }
}

/// Context dependency graph as (node names, directed edges) — the shared shape
/// fed to the pure-Rust centrality/community algorithms.
type ContextGraph = (Vec<String>, Vec<(String, String)>);

/// Derive a Rust module path from a source-file path: strip a leading `src/`,
/// drop the extension, and collapse `mod.rs`/`lib.rs`/`main.rs` to their
/// directory (crate root for lib/main). e.g. `src/domain/scanner.rs` →
/// `domain::scanner`, `src/domain/mod.rs` → `domain`, `src/lib.rs` → ``.
fn file_module_path(path: &str) -> String {
    let p = path.strip_prefix("./").unwrap_or(path);
    let rel = p.strip_prefix("src/").unwrap_or(p);
    let rel = rel.strip_suffix(".rs").unwrap_or(rel);
    let segs: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
    let mut mods: Vec<&str> = Vec::new();
    for (i, seg) in segs.iter().enumerate() {
        let is_last = i == segs.len() - 1;
        if is_last && (*seg == "mod" || *seg == "lib" || *seg == "main") {
            break;
        }
        mods.push(seg);
    }
    mods.join("::")
}

fn rust_surface_file_kind(path: &str) -> Option<&'static str> {
    let normalized = path.replace('\\', "/");
    if normalized == "src/lib.rs" || normalized.ends_with("/src/lib.rs") {
        Some("crate_root")
    } else if normalized == "mod.rs" || normalized.ends_with("/mod.rs") {
        Some("module_root")
    } else {
        None
    }
}

/// Resolve an import `to_module` use-path to an internal module path, relative to
/// the importing module. Only `crate::`/`super::`/`self::`-qualified paths are
/// internal; everything else (std, external crates) returns `None`. The resolved
/// candidate is matched to the longest known internal module that is a prefix.
fn resolve_internal_module(
    to_module: &str,
    from_mod: &str,
    known: &BTreeSet<String>,
) -> Option<String> {
    let segs: Vec<&str> = to_module.split("::").filter(|s| !s.is_empty()).collect();
    if segs.is_empty() {
        return None;
    }
    let from_segs: Vec<&str> = from_mod.split("::").filter(|s| !s.is_empty()).collect();
    let mut base: Vec<String>;
    let rest: Vec<&str>;
    match segs[0] {
        "crate" => {
            base = Vec::new();
            rest = segs[1..].to_vec();
        }
        "self" => {
            base = from_segs.iter().map(|s| s.to_string()).collect();
            rest = segs[1..].to_vec();
        }
        "super" => {
            base = from_segs.iter().map(|s| s.to_string()).collect();
            let mut i = 0;
            while i < segs.len() && segs[i] == "super" {
                base.pop();
                i += 1;
            }
            rest = segs[i..].to_vec();
        }
        _ => return None,
    }
    for seg in rest {
        if seg == "*" {
            continue;
        }
        base.push(seg.to_string());
    }
    let candidate = base.join("::");
    if candidate.is_empty() {
        return None;
    }
    known
        .iter()
        .filter(|m| !m.is_empty() && (**m == candidate || candidate.starts_with(&format!("{m}::"))))
        .max_by_key(|m| m.len())
        .cloned()
}

/// True if `ancestor` is a strict module-path ancestor of `descendant` (the
/// crate root, ``, is an ancestor of every non-root module).
fn is_ancestor(ancestor: &str, descendant: &str) -> bool {
    if ancestor.is_empty() {
        return !descendant.is_empty();
    }
    descendant.starts_with(&format!("{ancestor}::"))
}

fn module_parent_path(module: &str) -> Option<String> {
    module
        .rsplit_once("::")
        .map(|(parent, _)| parent.to_string())
}

fn module_path_reachable_without_edge(
    adjacency: &BTreeMap<String, BTreeSet<String>>,
    from: &str,
    to: &str,
) -> bool {
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::new();
    if let Some(next_modules) = adjacency.get(from) {
        for next in next_modules {
            if next != to {
                queue.push_back(next.clone());
            }
        }
    }
    while let Some(current) = queue.pop_front() {
        if current == to {
            return true;
        }
        if !seen.insert(current.clone()) {
            continue;
        }
        if let Some(next_modules) = adjacency.get(&current) {
            for next in next_modules {
                if !seen.contains(next) {
                    queue.push_back(next.clone());
                }
            }
        }
    }
    false
}

fn strongly_connected_module_components(
    nodes: Vec<String>,
    edges: &BTreeSet<(String, String)>,
) -> Vec<Vec<String>> {
    let nodes = nodes
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let index = nodes
        .iter()
        .enumerate()
        .map(|(idx, node)| (node.as_str(), idx))
        .collect::<HashMap<_, _>>();
    let mut adjacency = vec![Vec::new(); nodes.len()];
    for (from, to) in edges {
        let (Some(&from_idx), Some(&to_idx)) = (index.get(from.as_str()), index.get(to.as_str()))
        else {
            continue;
        };
        adjacency[from_idx].push(to_idx);
    }
    let mut reach = vec![BTreeSet::new(); nodes.len()];
    for start in 0..nodes.len() {
        let mut queue = VecDeque::from(adjacency[start].clone());
        while let Some(current) = queue.pop_front() {
            if reach[start].insert(current) {
                queue.extend(adjacency[current].iter().copied());
            }
        }
    }
    let mut visited = vec![false; nodes.len()];
    let mut components = Vec::new();
    for node_idx in 0..nodes.len() {
        if visited[node_idx] {
            continue;
        }
        visited[node_idx] = true;
        let mut component = vec![node_idx];
        for other_idx in (node_idx + 1)..nodes.len() {
            if !visited[other_idx]
                && reach[node_idx].contains(&other_idx)
                && reach[other_idx].contains(&node_idx)
            {
                visited[other_idx] = true;
                component.push(other_idx);
            }
        }
        if component.len() > 1 {
            components.push(
                component
                    .into_iter()
                    .map(|idx| nodes[idx].clone())
                    .collect::<Vec<_>>(),
            );
        }
    }
    components.sort();
    components
}

fn module_import_edge_betweenness(
    nodes: &BTreeSet<String>,
    edges: &BTreeSet<(String, String)>,
) -> Vec<((String, String), f64)> {
    let nodes = nodes.iter().cloned().collect::<Vec<_>>();
    let index = nodes
        .iter()
        .enumerate()
        .map(|(idx, node)| (node.as_str(), idx))
        .collect::<HashMap<_, _>>();
    let mut adjacency = vec![Vec::new(); nodes.len()];
    for (from, to) in edges {
        let (Some(&from_idx), Some(&to_idx)) = (index.get(from.as_str()), index.get(to.as_str()))
        else {
            continue;
        };
        adjacency[from_idx].push(to_idx);
    }

    let mut edge_scores: BTreeMap<(usize, usize), f64> = BTreeMap::new();
    for start in 0..nodes.len() {
        let mut stack = Vec::new();
        let mut predecessors = vec![Vec::new(); nodes.len()];
        let mut sigma = vec![0.0_f64; nodes.len()];
        sigma[start] = 1.0;
        let mut distance = vec![-1_i64; nodes.len()];
        distance[start] = 0;
        let mut queue = VecDeque::new();
        queue.push_back(start);
        while let Some(current) = queue.pop_front() {
            stack.push(current);
            for &next in &adjacency[current] {
                if distance[next] < 0 {
                    distance[next] = distance[current] + 1;
                    queue.push_back(next);
                }
                if distance[next] == distance[current] + 1 {
                    sigma[next] += sigma[current];
                    predecessors[next].push(current);
                }
            }
        }

        let mut dependency = vec![0.0_f64; nodes.len()];
        while let Some(target) = stack.pop() {
            if sigma[target] == 0.0 {
                continue;
            }
            for &predecessor in &predecessors[target] {
                let contribution =
                    (sigma[predecessor] / sigma[target]) * (1.0 + dependency[target]);
                *edge_scores.entry((predecessor, target)).or_default() += contribution;
                dependency[predecessor] += contribution;
            }
        }
    }

    let mut ranked = edge_scores
        .into_iter()
        .map(|((from_idx, to_idx), score)| {
            ((nodes[from_idx].clone(), nodes[to_idx].clone()), score)
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.0.0.cmp(&right.0.0))
            .then_with(|| left.0.1.cmp(&right.0.1))
    });
    ranked
}

fn undirected_module_adjacency(
    nodes: &BTreeSet<String>,
    edges: &BTreeSet<(String, String)>,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut adjacency = BTreeMap::new();
    for node in nodes {
        adjacency.entry(node.clone()).or_insert_with(BTreeSet::new);
    }
    for (from, to) in edges {
        if from == to {
            continue;
        }
        adjacency
            .entry(from.clone())
            .or_default()
            .insert(to.clone());
        adjacency
            .entry(to.clone())
            .or_default()
            .insert(from.clone());
    }
    adjacency
}

fn module_components_without(
    adjacency: &BTreeMap<String, BTreeSet<String>>,
    removed_node: Option<&str>,
    removed_edge: Option<(&str, &str)>,
) -> Vec<Vec<String>> {
    let mut visited = BTreeSet::new();
    let mut components = Vec::new();
    for node in adjacency.keys() {
        if removed_node == Some(node.as_str()) || visited.contains(node) {
            continue;
        }
        let mut component = Vec::new();
        let mut queue = VecDeque::from([node.clone()]);
        while let Some(current) = queue.pop_front() {
            if removed_node == Some(current.as_str()) || !visited.insert(current.clone()) {
                continue;
            }
            component.push(current.clone());
            if let Some(next_modules) = adjacency.get(&current) {
                for next in next_modules {
                    if removed_node == Some(next.as_str()) {
                        continue;
                    }
                    if let Some((left, right)) = removed_edge
                        && ((current == left && next == right)
                            || (current == right && next == left))
                    {
                        continue;
                    }
                    if !visited.contains(next) {
                        queue.push_back(next.clone());
                    }
                }
            }
        }
        if !component.is_empty() {
            component.sort();
            components.push(component);
        }
    }
    components.sort();
    components
}

fn module_articulation_separators(
    nodes: &BTreeSet<String>,
    edges: &BTreeSet<(String, String)>,
) -> Vec<(String, Vec<Vec<String>>)> {
    let adjacency = undirected_module_adjacency(nodes, edges);
    let baseline = module_components_without(&adjacency, None, None).len();
    let mut separators = adjacency
        .keys()
        .filter_map(|node| {
            let components = module_components_without(&adjacency, Some(node), None);
            (components.len() > baseline).then(|| (node.clone(), components))
        })
        .collect::<Vec<_>>();
    separators.sort_by(|left, right| {
        right
            .1
            .len()
            .cmp(&left.1.len())
            .then_with(|| left.0.cmp(&right.0))
    });
    separators
}

/// A module dependency edge, as an ordered `(from, to)` module-path pair.
type ModuleEdge = (String, String);

/// Connected components of the module graph, each a sorted list of module paths.
type ModuleComponents = Vec<Vec<String>>;

/// A bridge edge whose removal disconnects the module graph, with the resulting components.
type ModuleBridgeSeparator = (ModuleEdge, ModuleComponents);

fn module_bridge_separators(
    nodes: &BTreeSet<String>,
    edges: &BTreeSet<ModuleEdge>,
) -> Vec<ModuleBridgeSeparator> {
    let adjacency = undirected_module_adjacency(nodes, edges);
    let baseline = module_components_without(&adjacency, None, None).len();
    let mut undirected_edges = BTreeSet::new();
    for (from, to) in edges {
        if from <= to {
            undirected_edges.insert((from.clone(), to.clone()));
        } else {
            undirected_edges.insert((to.clone(), from.clone()));
        }
    }
    let mut separators = undirected_edges
        .into_iter()
        .filter_map(|(left, right)| {
            let components = module_components_without(&adjacency, None, Some((&left, &right)));
            (components.len() > baseline).then_some(((left, right), components))
        })
        .collect::<Vec<_>>();
    separators.sort_by(|left, right| {
        right
            .1
            .len()
            .cmp(&left.1.len())
            .then_with(|| left.0.0.cmp(&right.0.0))
            .then_with(|| left.0.1.cmp(&right.0.1))
    });
    separators
}

fn is_root_facade_import(to_module: &str, target_module: &str) -> bool {
    let segs: Vec<&str> = to_module.split("::").filter(|s| !s.is_empty()).collect();
    if segs.len() < 3 || segs.first() != Some(&"crate") {
        return false;
    }
    target_module == segs[1] && !target_module.contains("::")
}

fn public_api_like_module(module: &str) -> bool {
    let last = module.rsplit("::").next().unwrap_or(module);
    matches!(
        last,
        "api"
            | "contract"
            | "contracts"
            | "interface"
            | "interfaces"
            | "model"
            | "ports"
            | "prelude"
            | "protocol"
            | "types"
    )
}

fn common_non_project_callee(callee: &str) -> bool {
    matches!(
        callee,
        "and_then"
            | "as_ref"
            | "as_str"
            | "clone"
            | "collect"
            | "copied"
            | "default"
            | "entry"
            | "Err"
            | "expect"
            | "extend"
            | "filter"
            | "filter_map"
            | "fmt"
            | "get"
            | "insert"
            | "is_empty"
            | "iter"
            | "len"
            | "map"
            | "map_err"
            | "max"
            | "min"
            | "None"
            | "Ok"
            | "or_default"
            | "or_else"
            | "or_insert"
            | "push"
            | "Some"
            | "to_owned"
            | "to_string"
            | "unwrap"
            | "unwrap_or"
            | "unwrap_or_default"
    )
}

fn import_references_symbol(to_module: &str, symbol: &str) -> bool {
    to_module.split("::").any(|segment| segment == symbol)
}

fn type_references_symbol(type_name: &str, symbol: &str) -> bool {
    type_name
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .any(|segment| segment == symbol)
}

fn symbol_lookup_aliases(symbol: &str) -> Vec<String> {
    let mut aliases = vec![symbol.to_string()];
    if let Some(short) = symbol.rsplit("::").next()
        && short != symbol
    {
        aliases.push(short.to_string());
    }
    aliases
}

fn symbol_lookup_matches(stored: &str, requested: &str) -> bool {
    if stored == requested {
        return true;
    }
    let stored_short = stored.rsplit("::").next().unwrap_or(stored);
    let requested_short = requested.rsplit("::").next().unwrap_or(requested);
    stored_short == requested || stored == requested_short || stored_short == requested_short
}

fn generic_symbol_name(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "data"
            | "info"
            | "item"
            | "manager"
            | "helper"
            | "util"
            | "utils"
            | "handler"
            | "processor"
            | "service"
            | "run"
            | "process"
            | "handle"
            | "execute"
            | "get"
            | "set"
    )
}

fn rust_path_short_name(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path)
}

fn collect_dependency_paths(
    current: &str,
    target: &str,
    adjacency: &BTreeMap<String, Vec<String>>,
    visited: &mut BTreeSet<String>,
    path: &mut Vec<String>,
    paths: &mut Vec<Vec<String>>,
    max_depth: usize,
) {
    if current == target {
        paths.push(path.clone());
        return;
    }
    if path.len() > max_depth {
        return;
    }

    if let Some(next_contexts) = adjacency.get(current) {
        for next in next_contexts {
            if visited.contains(next) {
                continue;
            }
            visited.insert(next.clone());
            path.push(next.clone());
            collect_dependency_paths(next, target, adjacency, visited, path, paths, max_depth);
            path.pop();
            visited.remove(next);
        }
    }
}

fn rust_graph_schema_json() -> Value {
    json!({
        "node_relations": {
            "context": ["workspace", "name", "description", "module_path"],
            "module": ["workspace", "context", "name", "path", "public", "file_path", "description"],
            "source_file": ["workspace", "path", "context", "language"],
            "symbol": ["workspace", "name", "kind", "context", "file_path", "start_line", "end_line", "visibility"]
        },
        "edge_relations": {
            "context_dep": ["from_ctx", "to_ctx"],
            "import_edge": ["from_file", "to_module", "context"],
            "reference_edge": ["from_file", "to_path", "reference_kind", "line", "context"],
            "calls_symbol": ["caller", "callee", "file_path", "line", "context"],
            "ast_edge": ["from_node", "to_node", "edge_type"]
        },
        "query_views": ["overview", "relations", "nodes", "edges", "neighborhood", "paths"],
        "safety": "Structured graph views only; arbitrary Datalog is intentionally not exposed through MCP."
    })
}

/// Extract display string from a DataValue.
fn dv_str(val: &cozo::DataValue) -> String {
    match val {
        cozo::DataValue::Null => String::new(),
        cozo::DataValue::Bool(b) => b.to_string(),
        cozo::DataValue::Num(n) => match n {
            cozo::Num::Int(i) => i.to_string(),
            cozo::Num::Float(f) => f.to_string(),
        },
        cozo::DataValue::Str(s) => s.to_string(),
        cozo::DataValue::List(l) => {
            let items: Vec<String> = l.iter().map(dv_str).collect();
            format!("[{}]", items.join(", "))
        }
        _ => format!("{:?}", val),
    }
}

fn dv_i64(val: &cozo::DataValue) -> i64 {
    match val {
        cozo::DataValue::Num(cozo::Num::Int(i)) => *i,
        #[allow(clippy::cast_possible_truncation)]
        cozo::DataValue::Num(cozo::Num::Float(f)) => *f as i64,
        _ => 0,
    }
}

fn dv_opt_string(val: &cozo::DataValue) -> Option<String> {
    let value = dv_str(val);
    if value.is_empty() { None } else { Some(value) }
}

fn dv_opt_usize(val: &cozo::DataValue) -> Option<usize> {
    match dv_i64(val) {
        n if n > 0 => usize::try_from(n).ok(),
        _ => None,
    }
}

fn chrono_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let secs_per_day = 86400u64;
    let days = now / secs_per_day;
    let rem = now % secs_per_day;
    let hours = rem / 3600;
    let minutes = (rem % 3600) / 60;
    let seconds = rem % 60;
    let (year, month, day) = days_to_date(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, minutes, seconds
    )
}

fn days_to_date(days: u64) -> (u64, u64, u64) {
    let mut y = 1970;
    let mut remaining = days;
    loop {
        let days_in_year = if is_leap(y) { 366 } else { 365 };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        y += 1;
    }
    let month_days: &[u64] = if is_leap(y) {
        &[31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        &[31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut m = 1u64;
    for &md in month_days {
        if remaining < md {
            break;
        }
        remaining -= md;
        m += 1;
    }
    (y, m, remaining + 1)
}

fn is_leap(y: u64) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "cozo_tests.rs"]
mod tests;
