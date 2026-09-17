//! `rhaix` — командний рядок фреймворку.
//!
//! У M0 є лише `dev`: підняти сервер над текою проєкту. `new`, `check` і `build`
//! приїдуть у M6 і M8.

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "rhaix",
    version,
    about = "Серверний рендер HTMX на Rust + Rhai"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Запустити сервер розробки
    Dev {
        /// Корінь проєкту (там, де лежать pages/, layouts/, public/)
        #[arg(default_value = ".")]
        root: PathBuf,

        /// Порт
        #[arg(short, long, default_value_t = 3000)]
        port: u16,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rhaix=info,tower_http=warn".into()),
        )
        .with_target(false)
        .init();

    match Cli::parse().command {
        Command::Dev { root, port } => {
            // canonicalize на Windows повертає UNC-шлях `\\?\C:\...` — у виводі
            // це тільки заважає, тому префікс прибираємо.
            let root = root.canonicalize().unwrap_or_else(|_| root.clone());
            let root = match root.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
                Some(stripped) => PathBuf::from(stripped),
                None => root,
            };
            let addr = SocketAddr::from(([127, 0, 0, 1], port));
            rhaix_server::serve(rhaix_server::Config::new(root, addr)).await
        }
    }
}
