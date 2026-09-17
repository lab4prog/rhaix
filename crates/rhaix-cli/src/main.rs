//! `rhaix` — командний рядок фреймворку.
//!
//! `dev` — сервер розробки, `new` — скелет проєкту, `check` — перевірка всіх
//! `.rhx` без запуску. `build` приїде в M8.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

mod scaffold;

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

        /// Порт (сильніший за `rhaix.toml`)
        #[arg(short, long)]
        port: Option<u16>,
    },

    /// Створити новий проєкт
    New {
        /// Тека для проєкту
        path: PathBuf,
    },

    /// Перевірити всі `.rhx` проєкту, не запускаючи сервер
    Check {
        /// Корінь проєкту
        #[arg(default_value = ".")]
        root: PathBuf,

        /// Машинний вивід — для редакторів і агентів
        #[arg(long)]
        json: bool,
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
            let config = rhaix_server::Config::load(root, port)?;
            rhaix_server::serve(config).await
        }

        Command::New { path } => scaffold::create(&path),

        Command::Check { root, json } => {
            let config = rhaix_server::Config::load(root, None)?;
            let issues = rhaix_server::check(&config);

            if json {
                let body: Vec<String> = issues.iter().map(|issue| issue.to_json()).collect();
                println!("[{}]", body.join(","));
            } else if issues.is_empty() {
                println!("Помилок не знайдено.");
            } else {
                for issue in &issues {
                    println!("{}", issue.rendered);
                }
                println!("Знайдено проблем: {}", issues.len());
            }

            // Ненульовий код — щоб `rhaix check` можна було поставити в CI.
            if issues.is_empty() {
                Ok(())
            } else {
                std::process::exit(1);
            }
        }
    }
}
