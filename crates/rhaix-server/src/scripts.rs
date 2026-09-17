//! `scripts/*.rhai` — спільні функції проєкту.
//!
//! Без них єдиний спосіб поділитися функцією між сторінками — скопіювати її,
//! і саме це робить застосунок нечитабельним уже на п'ятій сторінці.
//!
//! Функції з цих файлів **доступні скрізь без жодного оголошення**: написали
//! `fn vat(x)` у `scripts/money.rhai` — і `vat(100)` працює і у frontmatter, і
//! в `{{ }}`. Це свідомо простіше за `import`: перше, що ми пообіцяли, —
//! працювати без знань про модулі.
//!
//! Чому не через `import`: Rhai тримає підключені модулі в стані виконання, а
//! не в scope. Frontmatter і вирази `{{ }}` виконуються окремими викликами,
//! тому `import "helpers" as h;` згори файлу до розмітки просто не доживав —
//! `{{ h::label(n) }}` падало з «Module not found». `import` лишається
//! робочим усередині одного блоку (див. [`ScriptResolver`]), але документуємо
//! ми автозавантаження.
//!
//! Власний резолвер, а не готовий `FileModuleResolver` із Rhai, з однієї
//! причини: файли проєкту ходять через трейт [`Files`], і у зібраному
//! бінарнику їх на диску немає взагалі.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rhai::module_resolvers::ModuleResolver;
use rhai::{Dynamic, Engine, EvalAltResult, Module, Position, Scope, Shared};
use rhaix_template::Files;

/// Тека, з якої беруться модулі. Шлях у `import` — це ім'я файлу без `.rhai`.
const SCRIPTS_DIR: &str = "scripts";

pub struct ScriptResolver {
    root: PathBuf,
    files: Arc<dyn Files>,
    /// У продакшні модуль компілюється один раз; у dev — щоразу, інакше
    /// правка `scripts/helpers.rhai` не була б видима без перезапуску.
    cache: Option<Mutex<BTreeMap<String, Shared<Module>>>>,
}

impl ScriptResolver {
    pub fn new(root: PathBuf, files: Arc<dyn Files>, dev: bool) -> Self {
        Self {
            root,
            files,
            cache: if dev { None } else { Some(Mutex::default()) },
        }
    }

    fn compile(&self, engine: &Engine, name: &str, pos: Position) -> Result<Shared<Module>, Box<EvalAltResult>> {
        // `ErrorModuleNotFound` друкує лише ім'я модуля й ковтає пояснення,
        // тому текст іде як звичайна помилка виконання — людині потрібен саме
        // шлях до файлу, якого не знайшли.
        let problem = |text: String| -> Box<EvalAltResult> {
            Box::new(EvalAltResult::ErrorRuntime(Dynamic::from(text), pos))
        };

        let path = self.path_of(name).ok_or_else(|| {
            problem(format!(
                "невідомий модуль `{name}`: у назві не можна використовувати `..` і абсолютні шляхи"
            ))
        })?;

        let source = self.files.read_text(&path).ok_or_else(|| {
            problem(format!(
                "невідомий модуль `{name}`: очікувався файл `{SCRIPTS_DIR}/{name}.rhai`"
            ))
        })?;

        let ast = engine
            .compile(&source)
            .map_err(|err| Box::new(EvalAltResult::from(err)))?;
        let module = Module::eval_ast_as_new(Scope::new(), &ast, engine)?;
        Ok(module.into())
    }

    /// Шлях модуля всередині проєкту. `None` — якщо ім'я намагається вийти
    /// за межі `scripts/`.
    fn path_of(&self, name: &str) -> Option<PathBuf> {
        let name = name.trim_start_matches("./");
        if name.is_empty()
            || name.starts_with('/')
            || name.starts_with('\\')
            || name.contains("..")
            || name.contains(':')
        {
            return None;
        }
        Some(self.root.join(SCRIPTS_DIR).join(format!("{name}.rhai")))
    }
}

impl ModuleResolver for ScriptResolver {
    fn resolve(
        &self,
        engine: &Engine,
        _source: Option<&str>,
        name: &str,
        pos: Position,
    ) -> Result<Shared<Module>, Box<EvalAltResult>> {
        let Some(cache) = &self.cache else {
            return self.compile(engine, name, pos);
        };
        if let Some(found) = cache.lock().expect("кеш модулів не отруєний").get(name) {
            return Ok(found.clone());
        }
        let module = self.compile(engine, name, pos)?;
        cache
            .lock()
            .expect("кеш модулів не отруєний")
            .insert(name.to_owned(), module.clone());
        Ok(module)
    }
}

/// Підключити всі `scripts/*.rhai` як глобальні функції рушія.
///
/// Помилка тут зупиняє старт: зламаний спільний скрипт має падати при запуску,
/// а не на першому запиті сторінки, яка ним користується.
pub fn load_globals(engine: &mut Engine, root: &Path, files: &dyn Files) -> anyhow::Result<usize> {
    let mut loaded = 0;
    for path in files.list(&root.join(SCRIPTS_DIR), "rhai") {
        let name = path.display().to_string();
        let source = files
            .read_text(&path)
            .ok_or_else(|| anyhow::anyhow!("{name}: не вдалося прочитати"))?;
        let ast = engine
            .compile(&source)
            .map_err(|err| anyhow::anyhow!("{name}: {err}"))?;
        let module = Module::eval_ast_as_new(Scope::new(), &ast, engine)
            .map_err(|err| anyhow::anyhow!("{name}: {err}"))?;
        engine.register_global_module(module.into());
        loaded += 1;
    }
    Ok(loaded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rhaix_template::EmbeddedFiles;

    fn resolver(dev: bool) -> ScriptResolver {
        static FILES: &[(&str, &[u8])] = &[(
            "scripts/helpers.rhai",
            b"fn twice(n) { n * 2 }\nlet GREETING = \"hi\";",
        )];
        ScriptResolver::new(PathBuf::new(), EmbeddedFiles::new(FILES), dev)
    }

    #[test]
    fn shared_functions_need_no_import() {
        static FILES: &[(&str, &[u8])] = &[("scripts/helpers.rhai", b"fn twice(n) { n * 2 }")];
        let files = EmbeddedFiles::new(FILES);
        let mut engine = rhaix_script::engine(rhaix_script::Limits::default());
        let loaded = load_globals(&mut engine, Path::new(""), files.as_ref()).expect("скрипти");

        assert_eq!(loaded, 1);
        assert_eq!(engine.eval::<i64>("twice(21)").expect("виклик"), 42);
    }

    #[test]
    fn a_broken_shared_script_stops_the_start() {
        static FILES: &[(&str, &[u8])] = &[("scripts/bad.rhai", b"fn oops( {")];
        let files = EmbeddedFiles::new(FILES);
        let mut engine = rhaix_script::engine(rhaix_script::Limits::default());
        let err = load_globals(&mut engine, Path::new(""), files.as_ref())
            .expect_err("зламаний скрипт має зупинити старт");
        assert!(err.to_string().contains("bad.rhai"), "{err}");
    }

    #[test]
    fn imported_functions_are_callable() {
        let mut engine = rhaix_script::engine(rhaix_script::Limits::default());
        engine.set_module_resolver(resolver(true));

        let value: i64 = engine
            .eval(r#"import "helpers" as h; h::twice(21)"#)
            .expect("модуль має підключитись");
        assert_eq!(value, 42);
    }

    #[test]
    fn a_missing_module_says_which_file_it_wanted() {
        let mut engine = rhaix_script::engine(rhaix_script::Limits::default());
        engine.set_module_resolver(resolver(true));

        let err = engine
            .eval::<i64>(r#"import "nope" as n; 1"#)
            .expect_err("такого модуля немає");
        assert!(err.to_string().contains("scripts/nope.rhai"), "{err}");
    }

    #[test]
    fn a_module_cannot_escape_the_scripts_directory() {
        let resolver = resolver(true);
        assert!(resolver.path_of("helpers").is_some());
        assert!(resolver.path_of("../../secrets").is_none());
        assert!(resolver.path_of("/etc/passwd").is_none());
        assert!(resolver.path_of("C:/windows").is_none());
    }

    #[test]
    fn production_compiles_a_module_once() {
        let resolver = resolver(false);
        let mut engine = rhaix_script::engine(rhaix_script::Limits::default());
        engine.set_module_resolver(resolver);
        // Двічі поспіль — другий раз має брати з кешу й дати той самий результат.
        for _ in 0..2 {
            let value: i64 = engine
                .eval(r#"import "helpers" as h; h::twice(2)"#)
                .expect("модуль");
            assert_eq!(value, 4);
        }
    }
}
