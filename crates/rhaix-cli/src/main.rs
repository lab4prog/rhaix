//! `rhaix` — командний рядок фреймворку.
//!
//! `dev` — сервер розробки, `serve` — продакшн з диска, `new` — скелет
//! проєкту, `build` — один бінарник, `check` — перевірка всіх `.rhx` без
//! запуску, `eject` — забрати вбудовану частину фреймворку в проєкт.

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

        /// Локальна тека фреймворку замість crates.io (для розробки самого rhaix)
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

    /// Забрати вбудовану частину фреймворку в проєкт, щоб правити її самому
    Eject {
        #[command(subcommand)]
        what: Eject,

        /// Корінь проєкту
        #[arg(long, default_value = ".", global = true)]
        root: PathBuf,

        /// Перезаписати файл, якщо він уже є
        #[arg(long, global = true)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum Eject {
    /// Тости й модальні вікна → public/rhaix-ui.js (замість вбудованого /_rhaix/ui.js)
    Ui,
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

        Command::Eject {
            what: Eject::Ui,
            root,
            force,
        } => eject_ui(&normalise(root), force),

        Command::Build {
            root,
            framework,
            out,
        } => {
            let root = normalise(root);
            let framework = match framework {
                Some(path) => build::Framework::Path(normalise(path)),
                None => detect_framework(),
            };
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

/// `rhaix eject ui`: покласти вбудований UI (тости, модалки) у
/// `public/rhaix-ui.js`. Щойно файл є, фреймворк підключає його замість
/// `/_rhaix/ui.js` — і далі це звичайний код проєкту.
fn eject_ui(root: &std::path::Path, force: bool) -> anyhow::Result<()> {
    if !root.join("rhaix.toml").is_file() && !root.join("pages").is_dir() {
        anyhow::bail!("у теці `{}` не схоже на проєкт rhaix", root.display());
    }
    let target = root.join("public").join("rhaix-ui.js");
    if target.exists() && !force {
        anyhow::bail!(
            "`{}` уже є — це і є ваш UI. Перезаписати вбудованим: --force",
            target.display()
        );
    }
    std::fs::create_dir_all(target.parent().expect("public/"))?;
    std::fs::write(&target, rhaix_server::UI_JS)?;
    println!("UI забрано в проєкт: {}", target.display());
    println!();
    println!("Тепер фреймворк підключає цей файл замість вбудованого /_rhaix/ui.js.");
    println!("Правте як завгодно; порожній файл вимикає тости й автомодалки зовсім.");
    Ok(())
}

/// canonicalize на Windows повертає verbatim-шлях виду `\\?\C:\...` — у виводі
/// це заважає, а в згенерованому `rhaix build` маніфесті стає `//?/C:/...`.
/// Префікс прибираємо лише перед буквою диска: `\\?\UNC\server\share` без
/// нього перестав би бути правильним шляхом.
fn normalise(path: PathBuf) -> PathBuf {
    let path = path.canonicalize().unwrap_or(path);
    strip_verbatim(path)
}

fn strip_verbatim(path: PathBuf) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path;
    };
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path,
    }
}

/// Звідки брати фреймворк, якщо `--framework` не вказано.
///
/// Раніше це завжди був шлях до репозиторію, з якого зібрано `rhaix`. Він
/// вшивається на етапі компіляції, тож у бінарника з релізу чи з
/// `cargo install` він указує на теку чужої машини (CI, реєстр cargo), і
/// `rhaix build` падав би в кожного, крім автора.
///
/// Тепер: якщо той репозиторій справді є на диску — це розробка самого rhaix,
/// беремо його; інакше — crates.io рівно тієї версії, що й цей CLI.
fn detect_framework() -> build::Framework {
    let checkout = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .map(PathBuf::from);
    match checkout {
        Some(root) if is_framework_checkout(&root) => build::Framework::Path(root),
        _ => build::Framework::Registry(env!("CARGO_PKG_VERSION").to_owned()),
    }
}

/// Чи це справді корінь репозиторію rhaix, а не випадкова тека з тим самим
/// відносним розташуванням (як-от `~/.cargo/registry/src/...`).
fn is_framework_checkout(root: &std::path::Path) -> bool {
    root.join("crates/rhaix-server/Cargo.toml").is_file()
        && root.join("crates/rhaix-template/Cargo.toml").is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_prefix_is_stripped_only_before_a_drive() {
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\C:\work\app")),
            PathBuf::from(r"C:\work\app")
        );
        // Мережевий шлях без префікса перестав би бути шляхом — лишаємо як є.
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\UNC\server\share")),
            PathBuf::from(r"\\?\UNC\server\share")
        );
        assert_eq!(
            strip_verbatim(PathBuf::from("/home/app")),
            PathBuf::from("/home/app")
        );
    }
}
