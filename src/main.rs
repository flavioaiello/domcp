mod domain;
mod mcp;
mod server;
mod store;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "domcp", about = "Domain Model Context Protocol Server")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the MCP stdio server (default when no subcommand given)
    Serve {
        /// Workspace path — auto-detected from VS Code via ${workspaceFolder}
        #[arg(short, long)]
        workspace: String,
    },

    /// Import a domcp.json file into the store for a workspace
    Import {
        /// Path to the JSON file to import
        file: String,

        /// Workspace path to associate with this model
        #[arg(short, long)]
        workspace: String,
    },

    /// Export a workspace's domain model to a JSON file
    Export {
        /// Output file path
        file: String,

        /// Workspace path whose model to export
        #[arg(short, long)]
        workspace: String,
    },

    /// List all projects stored in the local database
    List,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    match cli.command {
        // Default: serve
        None => {
            eprintln!("Usage: domcp serve --workspace <path>");
            eprintln!("       domcp import <file> --workspace <path>");
            eprintln!("       domcp export <file> --workspace <path>");
            eprintln!("       domcp list");
            std::process::exit(1);
        }

        Some(Commands::Serve { workspace }) => {
            let store = store::Store::open_default()?;

            let model = match store.load(&workspace)? {
                Some(m) => {
                    tracing::info!(
                        "Loaded model '{}' for workspace: {}",
                        m.name,
                        workspace
                    );
                    m
                }
                None => {
                    tracing::info!(
                        "No model found for workspace: {}. Starting with empty model.",
                        workspace
                    );
                    domain::model::DomainModel::empty(&workspace)
                }
            };

            tracing::info!(
                "DOMCP Server starting with {} bounded contexts, {} entities",
                model.bounded_contexts.len(),
                model
                    .bounded_contexts
                    .iter()
                    .map(|bc| bc.entities.len())
                    .sum::<usize>()
            );

            server::stdio::run(model, workspace, store).await?;
        }

        Some(Commands::Import { file, workspace }) => {
            let store = store::Store::open_default()?;
            let model = store.import_from_file(&workspace, &file)?;
            eprintln!(
                "Imported '{}' ({} contexts) into store for workspace: {}",
                model.name,
                model.bounded_contexts.len(),
                workspace
            );
        }

        Some(Commands::Export { file, workspace }) => {
            let store = store::Store::open_default()?;
            store.export_to_file(&workspace, &file)?;
            eprintln!("Exported model for workspace '{}' to: {}", workspace, file);
        }

        Some(Commands::List) => {
            let store = store::Store::open_default()?;
            let projects = store.list()?;
            if projects.is_empty() {
                eprintln!("No projects in store.");
            } else {
                eprintln!("{:<50} {:<25} {}", "WORKSPACE", "PROJECT", "UPDATED");
                eprintln!("{}", "-".repeat(95));
                for p in &projects {
                    eprintln!(
                        "{:<50} {:<25} {}",
                        p.workspace_path, p.project_name, p.updated_at
                    );
                }
                eprintln!("\n{} project(s) total", projects.len());
            }
        }
    }

    Ok(())
}
