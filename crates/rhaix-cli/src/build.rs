//! `rhaix build` — зібрати застосунок в один бінарник.
//!
//! Ідея проста: згенерувати крихітний Rust-крейт, який вшиває всі файли
//! проєкту через `include_bytes!` і піднімає сервер у режимі `embedded`.
//! Далі його збирає звичайний `cargo build --release`.
//!
//! Тому для збірки потрібен Rust-тулчейн — але тільки для неї. Результат
//! нічого не читає з диска, крім бази даних: деплой — це копіювання одного
//! файлу.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Що саме вшивати.
const EMBEDDED_EXTENSIONS: [&str; 3] = ["rhx", "sql", "rhai"];

pub fn build(root: &Path, framework: &Path, out: Option<PathBuf>) -> anyhow::Result<()> {
    if !root.join("rhaix.toml").is_file() && !root.join("pages").is_dir() {
        anyhow::bail!("у теці `{}` не схоже на проєкт rhaix", root.display());
    }

    let name = project_name(root);
    let workdir = out.unwrap_or_else(|| root.join("target").join("rhaix-build"));
    std::fs::create_dir_all(workdir.join("src"))?;

    let files = collect(root)?;
    if files.is_empty() {
        anyhow::bail!("нема чого вшивати: у проєкті не знайдено файлів");
    }

    std::fs::write(workdir.join("Cargo.toml"), manifest(&name, framework))?;
    std::fs::write(workdir.join("src/main.rs"), main_rs(root, &files))?;

    println!("Вшито файлів: {}", files.len());
    println!("Збірка: cargo build --release");

    let status = Command::new("cargo")
        .arg("build")
        .arg("--release")
        .current_dir(&workdir)
        .status()?;
    if !status.success() {
        anyhow::bail!("cargo build завершився з помилкою");
    }

    let produced = workdir
        .join("target")
        .join("release")
        .join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    let destination = root.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(&produced, &destination)?;

    let size = std::fs::metadata(&destination)
        .map(|m| m.len())
        .unwrap_or(0);
    println!();
    println!("Готово: {} ({} КБ)", destination.display(), size / 1024);
    println!("Запуск: {}", destination.display());
    println!("Порт береться з `rhaix.toml` або зі змінної PORT.");
    Ok(())
}

/// Ім'я застосунку — це ім'я теки, приведене до вигляду, який приймає cargo.
fn project_name(root: &Path) -> String {
    let raw = root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "rhaix-app".to_owned());
    let cleaned: String = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        format!("app-{cleaned}")
    } else {
        cleaned
    }
}

/// Усі файли проєкту, які потрапляють у бінарник.
fn collect(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    if root.join("rhaix.toml").is_file() {
        files.push(root.join("rhaix.toml"));
    }
    if root.join("middleware.rhx").is_file() {
        files.push(root.join("middleware.rhx"));
    }
    for dir in [
        "pages",
        "partials",
        "components",
        "layouts",
        "migrations",
        // `scripts/` теж: без нього `import "helpers";` у зібраному бінарнику
        // падав би на першому ж запиті.
        "scripts",
    ] {
        walk(&root.join(dir), &mut files, Some(&EMBEDDED_EXTENSIONS));
    }
    // `public/` іде цілком: там і css, і картинки, і шрифти.
    walk(&root.join("public"), &mut files, None);

    files.sort();
    Ok(files)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>, extensions: Option<&[&str]>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out, extensions);
            continue;
        }
        let keep = match extensions {
            Some(list) => path
                .extension()
                .and_then(|e| e.to_str())
                .map(|ext| list.contains(&ext))
                .unwrap_or(false),
            None => true,
        };
        if keep {
            out.push(path);
        }
    }
}

fn manifest(name: &str, framework: &Path) -> String {
    let framework = framework.to_string_lossy().replace('\\', "/");
    format!(
        r##"# Згенеровано `rhaix build`. Правити цей файл сенсу немає:
# наступна збірка перезапише його.
[package]
name = "{name}"
version = "0.1.0"
edition = "2021"

# Власний workspace: інакше cargo вирішить, що крейт належить до проєкту,
# усередині якого лежить тека target/.
[workspace]

[dependencies]
rhaix-server = {{ path = "{framework}/crates/rhaix-server" }}
rhaix-template = {{ path = "{framework}/crates/rhaix-template" }}
anyhow = "1"
tokio = {{ version = "1", features = ["rt-multi-thread", "macros", "net", "signal"] }}
tracing-subscriber = {{ version = "0.3", features = ["env-filter"] }}

[profile.release]
lto = "thin"
codegen-units = 1
strip = true

[[bin]]
name = "{name}"
path = "src/main.rs"
"##
    )
}

fn main_rs(root: &Path, files: &[PathBuf]) -> String {
    let mut entries = String::new();
    for file in files {
        let relative = file
            .strip_prefix(root)
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/");
        let absolute = file.to_string_lossy().replace('\\', "/");
        entries.push_str(&format!(
            "    (\"{relative}\", include_bytes!(r\"{absolute}\")),\n"
        ));
    }

    format!(
        r##"//! Згенеровано `rhaix build`. Усі файли проєкту вшиті нижче.

/// Файли проєкту: шлях відносно кореня → вміст.
static FILES: &[(&str, &[u8])] = &[
{entries}];

#[tokio::main]
async fn main() -> anyhow::Result<()> {{
    tracing_init();

    // Порт: спершу змінна оточення (зручно для контейнерів), далі `rhaix.toml`.
    let port = std::env::var("PORT").ok().and_then(|value| value.parse().ok());
    let files = rhaix_template::EmbeddedFiles::new(FILES);
    let config = rhaix_server::Config::embedded(files, port)?;
    rhaix_server::serve(config).await
}}

fn tracing_init() {{
    // Без цього бінарник мовчить: попередження про незаданий секрет, завелику
    // сесію чи загублений заголовок ішли б у нікуди. Знайдено в M5.2 —
    // зібраний застосунок не сказав ні слова про відсутній RHAIX_SECRET.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rhaix=info".into()),
        )
        .with_target(false)
        .init();
}}
"##
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_name_is_cargo_friendly() {
        assert_eq!(project_name(Path::new("/tmp/My App")), "my-app");
        assert_eq!(project_name(Path::new("/tmp/demo")), "demo");
        assert_eq!(project_name(Path::new("/tmp/2048")), "app-2048");
    }

    #[test]
    fn generated_main_lists_files_with_relative_paths() {
        let root = Path::new("C:/app");
        let code = main_rs(root, &[PathBuf::from("C:/app/pages/index.rhx")]);
        assert!(
            code.contains("(\"pages/index.rhx\", include_bytes!"),
            "{code}"
        );
        assert!(code.contains("C:/app/pages/index.rhx"), "{code}");
        assert!(code.contains("Config::embedded"), "{code}");
        // Зібраний застосунок має вміти говорити: інакше попередження про
        // незаданий секрет нікуди не потрапляє.
        assert!(code.contains("tracing_subscriber::fmt()"), "{code}");
    }

    #[test]
    fn manifest_declares_its_own_workspace() {
        let text = manifest("demo", Path::new("C:/rhaix"));
        assert!(text.contains("[workspace]"), "{text}");
        assert!(text.contains("C:/rhaix/crates/rhaix-server"), "{text}");
        assert!(text.contains("tracing-subscriber"), "{text}");
    }
}
