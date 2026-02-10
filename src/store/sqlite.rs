use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

use crate::domain::model::DomainModel;

/// SQLite-backed store for domain models, keyed by workspace path.
/// Database lives at `~/.domcp/domcp.db`.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (or create) the store at the default location `~/.domcp/domcp.db`.
    pub fn open_default() -> Result<Self> {
        let db_path = default_db_path()?;
        Self::open(&db_path)
    }

    /// Open (or create) the store at a specific path.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }

        let conn = Connection::open(path)
            .with_context(|| format!("Failed to open database: {}", path.display()))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS projects (
                workspace_path TEXT PRIMARY KEY,
                project_name   TEXT NOT NULL,
                model_json     TEXT NOT NULL,
                created_at     TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at     TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )
        .context("Failed to initialize database schema")?;

        Ok(Self { conn })
    }

    /// Load the domain model for a workspace. Returns `None` if no model exists.
    pub fn load(&self, workspace_path: &str) -> Result<Option<DomainModel>> {
        let canonical = canonicalize_path(workspace_path);
        let mut stmt = self
            .conn
            .prepare("SELECT model_json FROM projects WHERE workspace_path = ?1")?;

        let result = stmt.query_row([&canonical], |row| {
            let json: String = row.get(0)?;
            Ok(json)
        });

        match result {
            Ok(json) => {
                let model: DomainModel = serde_json::from_str(&json)
                    .context("Failed to parse stored domain model")?;
                Ok(Some(model))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("Failed to query project"),
        }
    }

    /// Save (upsert) a domain model for a workspace.
    pub fn save(&self, workspace_path: &str, model: &DomainModel) -> Result<()> {
        let canonical = canonicalize_path(workspace_path);
        let json = serde_json::to_string_pretty(model)
            .context("Failed to serialize domain model")?;

        self.conn.execute(
            "INSERT INTO projects (workspace_path, project_name, model_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, datetime('now'), datetime('now'))
             ON CONFLICT(workspace_path) DO UPDATE SET
                 project_name = excluded.project_name,
                 model_json   = excluded.model_json,
                 updated_at   = datetime('now')",
            [&canonical, &model.name, &json],
        )
        .context("Failed to save domain model")?;

        Ok(())
    }

    /// List all stored projects with their workspace paths and names.
    pub fn list(&self) -> Result<Vec<ProjectInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT workspace_path, project_name, updated_at FROM projects ORDER BY updated_at DESC",
        )?;

        let rows = stmt
            .query_map([], |row| {
                Ok(ProjectInfo {
                    workspace_path: row.get(0)?,
                    project_name: row.get(1)?,
                    updated_at: row.get(2)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(rows)
    }

    /// Import a domain model from a JSON file into the store for a given workspace.
    /// Validates the model before storing (name required, entity names non-empty).
    pub fn import_from_file(&self, workspace_path: &str, file_path: &str) -> Result<DomainModel> {
        let model = DomainModel::load(file_path)?;
        self.save(workspace_path, &model)?;
        Ok(model)
    }

    /// Export a domain model from the store to a JSON file.
    pub fn export_to_file(&self, workspace_path: &str, file_path: &str) -> Result<()> {
        let model = self
            .load(workspace_path)?
            .with_context(|| format!("No model found for workspace: {workspace_path}"))?;
        let json = serde_json::to_string_pretty(&model)?;
        std::fs::write(file_path, json)
            .with_context(|| format!("Failed to write file: {file_path}"))?;
        Ok(())
    }
}

/// Metadata about a stored project.
#[derive(Debug, Clone)]
pub struct ProjectInfo {
    pub workspace_path: String,
    pub project_name: String,
    pub updated_at: String,
}

/// Returns the default database path: `~/.domcp/domcp.db`
fn default_db_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    Ok(home.join(".domcp").join("domcp.db"))
}

/// Normalize workspace path for consistent keying.
fn canonicalize_path(path: &str) -> String {
    let normalized = path.trim_end_matches('/');
    // Try to resolve symlinks / relative segments; fall back to stripped path
    match std::fs::canonicalize(normalized) {
        Ok(p) => p.to_string_lossy().to_string(),
        Err(_) => normalized.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::model::*;
    use std::env::temp_dir;

    fn test_model(name: &str) -> DomainModel {
        DomainModel {
            name: name.to_string(),
            description: "Test project".into(),
            bounded_contexts: vec![],
            rules: vec![],
            tech_stack: TechStack::default(),
            conventions: Conventions::default(),
        }
    }

    fn temp_store() -> Store {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = temp_dir()
            .join(format!("domcp_test_{}_{}.db", std::process::id(), id));
        Store::open(&path).unwrap()
    }

    #[test]
    fn test_save_and_load() {
        let store = temp_store();
        let model = test_model("TestProject");
        store.save("/tmp/my-project", &model).unwrap();

        let loaded = store.load("/tmp/my-project").unwrap().unwrap();
        assert_eq!(loaded.name, "TestProject");
    }

    #[test]
    fn test_load_nonexistent() {
        let store = temp_store();
        let result = store.load("/tmp/does-not-exist").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_list_projects() {
        let store = temp_store();
        store.save("/tmp/proj-a", &test_model("ProjectA")).unwrap();
        store.save("/tmp/proj-b", &test_model("ProjectB")).unwrap();

        let projects = store.list().unwrap();
        assert_eq!(projects.len(), 2);
    }

    #[test]
    fn test_upsert() {
        let store = temp_store();
        store.save("/tmp/my-project", &test_model("V1")).unwrap();
        store.save("/tmp/my-project", &test_model("V2")).unwrap();

        let projects = store.list().unwrap();
        assert_eq!(projects.len(), 1);

        let loaded = store.load("/tmp/my-project").unwrap().unwrap();
        assert_eq!(loaded.name, "V2");
    }
}
