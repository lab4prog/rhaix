//! `rhaix` — командний рядок фреймворку.
//!
//! `dev` — сервер розробки, `new` — скелет проєкту, `check` — перевірка всіх
//! `.rhx` без запуску. `build` приїде в M8.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

mod build;
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

    /// Запустити сервер у режимі продакшну
    Serve {
        /// Корінь проєкту
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

    /// Зібрати застосунок в один бінарник
    Build {
        /// Корінь проєкту
        #[arg(default_value = ".")]
        root: PathBuf,

        /// Де лежить сам фреймворк (поки він не в crates.io)
        #[arg(long)]
        framework: Option<PathBuf>,

        /// Тека для згенерованого крейта
        #[arg(long)]
        out: Option<PathBuf>,
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
            let root = normalise(root);
            let config = rhaix_server::Config::load(root, port)?;
            rhaix_server::serve(config).await
        }

        Command::Serve { root, port } => {
            let root = normalise(root);
            let config = rhaix_server::Config::load_release(root, port)?;
            rhaix_server::serve(config).await
        }

        Command::New { path } => scaffold::create(&path),

        Command::Build {
            root,
            framework,
            out,
        } => {
            let root = normalise(root);
            // Шлях до фреймворку відомий на момент компіляції самого `rhaix`:
            // поки крейти не опубліковані, згенерований проєкт посилається
            // на цей репозиторій.
            let framework = framework.unwrap_or_else(default_framework);
            build::build(&root, &framework, out)
        }

        Command::Check { root, json } => {
            let config = rhaix_server::Config::load_for_check(root)?;
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

/// canonicalize на Windows повертає UNC-шлях виду `\?\C:\...` — у виводі це
/// лише заважає, тому префікс прибираємо.
fn normalise(path: PathBuf) -> PathBuf {
    let path = path.canonicalize().unwrap_or(path);
    match path.to_str().and_then(|text| text.strip_prefix(r"\?\")) {
        Some(stripped) => PathBuf::from(stripped),
        None => path,
    }
}

/// Тека фреймворку за замовчуванням — той репозиторій, з якого зібрано `rhaix`.
fn default_framework() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}
