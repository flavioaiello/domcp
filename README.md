# DOMCP — Domain Model Context Protocol Server

A Rust-based MCP server that feeds **domain model abstractions** into GitHub Copilot, ensuring AI-generated code follows your architecture, conventions, and domain-driven design patterns.

## Why DOMCP?

### Copilot has no memory across sessions

Without DOMCP, every new chat starts from zero. Copilot re-discovers your architecture by reading files — slowly, incompletely, and inconsistently. DOMCP gives it the full domain model in **few tokens** (one tool call), which is faster and cheaper than Copilot scanning 50 files to piece it together.

### Copilot doesn't enforce architectural boundaries

Left alone, Copilot will happily create a direct import from your domain layer into infrastructure, or skip aggregate roots entirely. DOMCP's `validate_dependency` and `get_architectural_rules` act as **guardrails that Copilot checks before generating code**. This is the highest-value feature — preventing architectural drift is expensive to fix later.

### The bidirectional flow solves a real onboarding problem

"Analyze this codebase and document its domain model" is something teams do manually in wikis that go stale. Having Copilot do it and persist it to `domcp.json` means the model stays **machine-readable and version-controlled** alongside the code.

## How It Works

```
┌─────────────────────────────────────────────────────┐
│  GitHub Copilot (VS Code)                           │
│                                                     │
│  "Create a new billing endpoint"                    │
│       │                                             │
│       ▼                                             │
│  ┌──────────────┐    MCP stdio   ┌───────────────┐  │
│  │ Copilot Chat │◄──────────────►│ DOMCP Server  │  │
│  │ / Agent      │                │               │  │
│  └──────────────┘                │ ▪ Entities    │  │
│       │                          │ ▪ Services    │  │
│       ▼                          │ ▪ Rules       │  │
│  Code that follows YOUR          │ ▪ Conventions │  │
│  architecture & conventions      └───────────────┘  │
└─────────────────────────────────────────────────────┘
```

## Quick Start

### 1. Install via Homebrew

```bash
brew tap flavioaiello/domcp git@github.com:flavioaiello/domcp.git
brew install domcp
```

Or build from source:

```bash
cargo build --release
cargo install --path .
```

### 2. Import Your Domain Model (optional)

If you have an existing `domcp.json`, import it into the local store:

```bash
domcp import domcp.json --workspace /path/to/your/project
```

The model is stored in `~/.domcp/domcp.db` (SQLite), keyed by workspace path.
If you skip this step, DOMCP starts with an empty model that Copilot can populate via write tools.

### 3. Integrate with VS Code / GitHub Copilot

Add to your project's `.vscode/mcp.json`:

```json
{
    "servers": {
        "domcp": {
            "type": "stdio",
            "command": "domcp",
            "args": ["serve", "--workspace", "${workspaceFolder}"]
        }
    }
}
```

After installing, **restart VS Code** or run `> MCP: List Servers` from the command palette to see the DOMCP server listed and active.

### CLI Commands

```bash
# Start MCP server (used by VS Code, not called manually)
domcp serve --workspace /path/to/project

# Import a domcp.json file into the local store
domcp import domcp.json --workspace /path/to/project

# Export a project's model back to JSON
domcp export model.json --workspace /path/to/project

# List all stored projects
domcp list
```

## How It Works with Copilot

Once connected, Copilot gains access to **16 tools** (8 read, 8 write), **1 prompt**, and **dynamic resources**:

### Read Tools (query the domain model)

| Tool | What it does |
|------|-------------|
| `get_architecture_overview` | Full architecture summary — Copilot reads this to understand the system |
| `get_bounded_context` | Details of a specific bounded context |
| `get_entity` | Entity spec with fields, methods, invariants |
| `get_service_spec` | Service definition with methods, deps, layer |
| `validate_dependency` | Checks if a cross-context dependency is allowed |
| `get_architectural_rules` | All rules code must follow |
| `get_conventions` | Naming, file structure, error handling patterns |
| `suggest_file_path` | Where a new file should be placed per conventions |

### Write Tools (update the domain model)

| Tool | What it does |
|------|-------------|
| `update_bounded_context` | Create or update a bounded context |
| `update_entity` | Create or merge an entity (fields, methods, invariants) |
| `update_service` | Create or update a service within a context |
| `update_event` | Create or update a domain event |
| `remove_entity` | Remove an entity from a context |
| `compare_model` | Diff in-memory model vs persisted → list of changes |
| `draft_refactoring_plan` | Diff in-memory model vs persisted → code actions, file paths, priorities, migration notes |
| `save_model` | Persist the current model to the local store |

### Resources (Copilot can attach these as context)

| URI | Content |
|-----|---------|
| `domcp://architecture/overview` | Architecture overview (JSON) |
| `domcp://architecture/rules` | Architectural rules (JSON) |
| `domcp://architecture/conventions` | Conventions (JSON) |
| `domcp://context/{name}` | Per bounded-context detail (JSON) |

### Prompt

| Name | Description |
|------|-------------|
| `domcp_guidelines` | Architecture guidelines and mandatory tool-usage workflow. Renders project-specific content (project name, bounded context list, 11-step workflow, DDD rules). Eliminates the need for a per-project `copilot-instructions.md`. |

### Example Copilot Interactions

**You ask:** *"Create a new endpoint to cancel a subscription"*

Copilot will:
1. Call `get_architecture_overview` → learns the system has Identity and Billing contexts
2. Call `get_entity("Subscription")` → sees it's an aggregate root in Billing with a `cancel()` method
3. Call `get_conventions` → learns file structure pattern `src/{context}/{layer}/{type}.rs`
4. Call `suggest_file_path("Billing", "service", "CancelSubscription")` → `src/billing/application/cancel_subscription.rs`
5. Call `validate_dependency("Billing", "Identity")` → allowed
6. Generate code that:
   - Places the handler in `src/billing/api/`
   - Uses the `Subscription` aggregate's `cancel()` method
   - Emits a domain event
   - Follows error handling conventions (`thiserror`)
   - Respects the repository pattern

**You ask:** *"Add a field to User"*

Copilot will:
1. Call `get_entity("User")` → sees existing fields, invariants  
2. Call `get_architectural_rules` → knows mutations go through aggregate root methods
3. Generate code that modifies the `User` struct AND adds the corresponding migration, event update, and test

### Bidirectional: Codebase → Model → Refactoring

**You ask:** *"Analyze this codebase and build a domain model from it"*

Copilot will:
1. Scan the module structure → call `update_bounded_context` for each discovered context
2. Read entity files → call `update_entity` with fields, methods, invariants
3. Read service files → call `update_service` with dependencies and layer
4. Call `save_model` to persist everything to the local store

**You then ask:** *"Rename the Identity context to Auth and add a `last_login` field to User"*

Copilot will:
1. Call `update_bounded_context` to rename/update the context
2. Call `update_entity` to add the field
3. Call `compare_model` → sees the diff between in-memory and persisted models
4. Call `draft_refactoring_plan` → gets a prioritized list of code changes:
   - `modify_file: src/identity/domain/user.rs` (high)
   - `move_file: src/identity → src/auth` (critical)
   - Migration note: *"New field 'last_login' on 'User' — needs ALTER TABLE migration"*
5. Execute code actions in priority order
6. Call `save_model` to persist the updated model to the local store

## Domain Model Schema

The `domcp.json` file describes your entire system architecture:

```
DomainModel
├── name, description
├── tech_stack (language, framework, database, ...)
├── bounded_contexts[]
│   ├── name, module_path
│   ├── entities[] (fields, methods, invariants, aggregate_root)
│   ├── value_objects[] (fields, validation_rules)
│   ├── services[] (kind: domain|application|infrastructure, methods, dependencies)
│   ├── repositories[] (aggregate, methods)
│   ├── events[] (fields, source entity)
│   └── dependencies[] (allowed cross-context deps)
├── rules[] (id, description, severity, scope)
└── conventions
    ├── naming (entities, services, events, ...)
    ├── file_structure (pattern, layers)
    ├── error_handling
    └── testing
```

## Storage

DOMCP stores domain models in a local SQLite database at `~/.domcp/domcp.db`, keyed by workspace path. This means:

- **Multi-project support**: Each workspace gets its own isolated model
- **No per-project config files needed**: The model lives centrally on the dev machine
- **Portable import/export**: Use `domcp import` / `export` to share models via `domcp.json` files
- **Version control friendly**: Export to `domcp.json` when you want to commit the model to git

## Architectural Enforcement

DOMCP doesn't just inform — it **constrains**. The `validate_dependency` tool lets Copilot check whether cross-context imports are allowed before generating them. The architectural rules describe invariants that Copilot will respect.

Example rules from the included config:
- **LAYER-001**: Domain layer must not depend on infrastructure
- **DDD-001**: State mutations must go through aggregate root methods
- **DDD-002**: Cross-aggregate communication via domain events only
- **ERR-001**: Use typed domain errors, never panic

## Advanced: Custom `instructions.md`

DOMCP ships a built-in `domcp_guidelines` prompt that serves architecture instructions automatically. For additional project-specific instructions, create `.github/copilot-instructions.md`:

```markdown
## Architecture

This project uses Domain-Driven Design with a hexagonal architecture.
Before writing any code, ALWAYS call `get_architecture_overview` from the DOMCP
server to understand the system structure.

When creating new files, call `suggest_file_path` to determine the correct location.
When adding cross-context dependencies, call `validate_dependency` to verify it's allowed.
Always check `get_conventions` for naming and error handling patterns.
```

This ensures Copilot **proactively** queries the domain model rather than waiting for tool hints.

## Installation

### Homebrew (recommended)

```bash
brew tap flavioaiello/domcp git@github.com:flavioaiello/domcp.git
brew install domcp
```

### From source

```bash
cargo install --path .
```

## Development

```bash
# Build debug
cargo build

# Run tests
cargo test

# Run with debug logging
RUST_LOG=debug cargo run -- serve --workspace .

# Import the example model
cargo run -- import domcp.json --workspace /path/to/project

# List stored projects
cargo run -- list
```

## License

MIT
