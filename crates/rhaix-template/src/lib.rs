//! Шаблонізатор `.rhx`: розбір, компіляція виразів і рендер.
//!
//! ```text
//! .rhx → split (frontmatter | розмітка) → parse → Template (незмінний, Arc)
//!                                                     ↓ на кожен запит
//!                                              render(Scope) → HTML
//! ```
//!
//! Вирази шаблону — це Rhai: кожен `{{ ... }}` компілюється один раз у
//! `rhai::AST` і далі лише виконується. Позиції помилок переносяться в
//! координати `.rhx`, тому користувач ніколи не бачить сирого Rhai.

pub mod ast;
pub mod error;
pub mod escape;
mod expr;
mod files;
mod loader;
mod parse;
mod render;

use std::sync::Arc;

use rhai::{Dynamic, Engine, Scope};
use rhaix_parser::{Source, Span};

pub use ast::Node;
pub use error::Diagnostic;
pub use files::{DiskFiles, EmbeddedFiles, Files};
pub use loader::{Components, Loader, NoComponents, TemplateCache};
pub use render::{Asset, CsrfToken, Globals, Rendered, Slots};
pub use rhaix_script::{CSRF_FIELD, CSRF_HEADER};

use expr::Expr;

/// Скомпільований шаблон. Спільний для всіх запитів: на запит змінюється
/// тільки `Scope`.
#[derive(Debug, Clone)]
pub struct Template {
    source: Arc<Source>,
    nodes: Vec<Node>,
    frontmatter: Option<Span>,
    /// Скомпільований frontmatter. Компілюється разом із розміткою, тому
    /// синтаксична помилка в логіці видно одразу, а не на першому запиті.
    script: Option<Expr>,
}

impl Template {
    /// Скомпілювати файл без компонентів — для тестів і найпростіших сторінок.
    pub fn compile(source: Arc<Source>, engine: &Engine) -> Result<Self, Diagnostic> {
        Self::compile_with(source, engine, &NoComponents)
    }

    /// Скомпілювати файл, резолвлячи компоненти через `components`.
    pub fn compile_with(
        source: Arc<Source>,
        engine: &Engine,
        components: &dyn Components,
    ) -> Result<Self, Diagnostic> {
        let split = rhaix_parser::split(&source).map_err(|err| {
            let diagnostic = Diagnostic::new(err.message, err.span);
            let diagnostic = match err.hint {
                Some(hint) => diagnostic.with_hint(hint),
                None => diagnostic,
            };
            diagnostic.in_file(source.clone())
        })?;
        let nodes = parse::parse(&source, engine, components, split.markup)
            .map_err(|diagnostic| diagnostic.in_file(source.clone()))?;
        let script = match split.frontmatter {
            Some(span) if !source.slice(span).trim().is_empty() => Some(
                Expr::compile_script(engine, &source, span)
                    .map_err(|diagnostic| diagnostic.in_file(source.clone()))?,
            ),
            _ => None,
        };
        Ok(Self {
            source,
            nodes,
            frontmatter: split.frontmatter,
            script,
        })
    }

    /// Виконати frontmatter.
    ///
    /// Повертає значення, яке віддав скрипт: `()` означає «рендери розмітку»,
    /// будь-що інше — готове тіло відповіді (`return "";` — порожнє).
    pub fn run_script(&self, engine: &Engine, scope: &mut Scope) -> Result<Dynamic, Diagnostic> {
        match &self.script {
            Some(script) => script
                .eval(engine, scope)
                .map_err(|diagnostic| diagnostic.in_file(self.source.clone())),
            None => Ok(Dynamic::UNIT),
        }
    }

    pub fn has_script(&self) -> bool {
        self.script.is_some()
    }

    /// Rhai-код frontmatter. Виконання з'явиться в M2.
    pub fn frontmatter(&self) -> Option<&str> {
        self.frontmatter.map(|span| self.source.slice(span))
    }

    pub fn source(&self) -> &Source {
        &self.source
    }

    /// Джерело як `Arc` — щоб діагностика могла нести його з собою.
    pub fn source_arc(&self) -> Arc<Source> {
        self.source.clone()
    }

    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    pub fn render<'a>(
        &'a self,
        engine: &'a Engine,
        scope: &mut Scope,
        slots: Slots<'a>,
        globals: &'a Globals,
    ) -> Result<Rendered, Diagnostic> {
        self.render_with(engine, scope, slots, globals, true)
    }

    /// Рендер із вибором: піднімати вбудовані `<style>`/`<script>` чи ні.
    /// Layout рендериться без підйому — він сам є документом.
    pub fn render_with<'a>(
        &'a self,
        engine: &'a Engine,
        scope: &mut Scope,
        slots: Slots<'a>,
        globals: &'a Globals,
        hoist: bool,
    ) -> Result<Rendered, Diagnostic> {
        render::render_with(
            &self.source,
            engine,
            scope,
            &self.nodes,
            slots,
            globals,
            hoist,
        )
        .map_err(|diagnostic| diagnostic.in_file(self.source.clone()))
    }

    /// Готовий текст помилки з підсвіченим рядком файлу.
    pub fn describe(&self, diagnostic: &Diagnostic) -> String {
        diagnostic.render(&self.source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rhai::Dynamic;
    use rhaix_script::{engine, Limits};

    fn render_with(text: &str, fill: impl FnOnce(&mut Scope)) -> Result<String, String> {
        let engine = engine(Limits::default());
        let source = Arc::new(Source::new("test.rhx", text));
        let template = Template::compile(source, &engine)
            .map_err(|d| d.render(&Source::new("test.rhx", text)))?;
        let mut scope = Scope::new();
        fill(&mut scope);
        template
            .render(&engine, &mut scope, Slots::default(), &Globals::default())
            .map(|rendered| rendered.html)
            .map_err(|d| template.describe(&d))
    }

    fn render(text: &str) -> String {
        render_with(text, |_| {}).expect("шаблон має рендеритись")
    }

    fn error(text: &str) -> String {
        render_with(text, |_| {}).expect_err("очікувалась помилка")
    }

    fn todos() -> Dynamic {
        let mut first = rhai::Map::new();
        first.insert("title".into(), Dynamic::from("Купити молоко"));
        first.insert("done".into(), Dynamic::from(true));
        let mut second = rhai::Map::new();
        second.insert("title".into(), Dynamic::from("Зробити домашку"));
        second.insert("done".into(), Dynamic::from(false));
        Dynamic::from(vec![Dynamic::from_map(first), Dynamic::from_map(second)])
    }

    // ------------------------------------------------------------- вивід

    #[test]
    fn plain_markup_passes_through() {
        assert_eq!(render("<h1>Привіт</h1>"), "<h1>Привіт</h1>");
    }

    #[test]
    fn interpolation_is_escaped_by_default() {
        let html = render_with("<p>{{ name }}</p>", |scope| {
            scope.push("name", "<script>alert(1)</script>");
        })
        .unwrap();
        assert_eq!(html, "<p>&lt;script&gt;alert(1)&lt;/script&gt;</p>");
    }

    #[test]
    fn raw_opts_out_of_escaping() {
        let html = render_with("<p>{{ raw(body) }}</p>", |scope| {
            scope.push("body", "<b>жирний</b>");
        })
        .unwrap();
        assert_eq!(html, "<p><b>жирний</b></p>");
    }

    #[test]
    fn unit_and_false_render_as_nothing() {
        let html = render_with("<p>{{ missing }}|{{ flag }}</p>", |scope| {
            scope.push("missing", ());
            scope.push("flag", false);
        })
        .unwrap();
        assert_eq!(html, "<p>|</p>");
    }

    #[test]
    fn comments_do_not_reach_the_output() {
        assert_eq!(render("<p>a{{! таємниця }}b</p>"), "<p>ab</p>");
    }

    #[test]
    fn trim_markers_remove_whitespace() {
        let html = render_with("<td>\n  {{- total -}}\n</td>", |scope| {
            scope.push("total", 1500_i64);
        })
        .unwrap();
        assert_eq!(html, "<td>1500</td>");
    }

    #[test]
    fn raw_block_is_verbatim() {
        assert_eq!(
            render("<rhaix:raw>{{ не вираз }}</rhaix:raw>"),
            "{{ не вираз }}"
        );
    }

    // -------------------------------------------------------- директиви

    #[test]
    fn if_else_chain_picks_one_branch() {
        let template = "<p @if={role == \"admin\"}>Адмін</p>\n<p @else-if={role == \"editor\"}>Редактор</p>\n<p @else>Гість</p>";
        let html = render_with(template, |scope| {
            scope.push("role", "editor");
        })
        .unwrap();
        assert_eq!(html, "<p>Редактор</p>");
    }

    #[test]
    fn real_text_between_branches_breaks_the_chain() {
        // Між `@if` і `@else` дозволені лише пробіли (SYNTAX 4.1). Якщо там
        // справжній текст, ланцюжок обривається — і `@else` лишається сиротою.
        // Краще зрозуміла помилка, ніж тихо проковтнутий текст.
        let message = error("<p @if={show}>так</p> між гілками <p @else>ні</p>");
        assert!(message.contains("`@else` без `@if`"), "{message}");
    }

    #[test]
    fn whitespace_between_branches_is_dropped() {
        let html = render_with(
            "<p @if={show}>так</p>
  <p @else>ні</p>",
            |scope| {
                scope.push("show", false);
            },
        )
        .unwrap();
        assert_eq!(html, "<p>ні</p>");
    }

    #[test]
    fn soft_truthiness_hides_zero_and_empty() {
        let html = render_with("<p @if={count}>є</p><p @else>немає</p>", |scope| {
            scope.push("count", 0_i64);
        })
        .unwrap();
        assert_eq!(html, "<p>немає</p>");
    }

    #[test]
    fn for_renders_every_item() {
        let html = render_with("<li @for={t in todos}>{{ t.title }}</li>", |scope| {
            scope.push_dynamic("todos", todos());
        })
        .unwrap();
        assert_eq!(html, "<li>Купити молоко</li><li>Зробити домашку</li>");
    }

    #[test]
    fn for_exposes_iteration_counters() {
        let html = render_with(
            "<li @for={t in todos}>{{ iter.number }}/{{ iter.count }}{{ if iter.last { \"!\" } else { \"\" } }}</li>",
            |scope| { scope.push_dynamic("todos", todos()); },
        )
        .unwrap();
        assert_eq!(html, "<li>1/2</li><li>2/2!</li>");
    }

    #[test]
    fn for_with_index_and_ranges() {
        let html = render_with("<b @for={(t, i) in todos}>{{ i }}</b>", |scope| {
            scope.push_dynamic("todos", todos());
        })
        .unwrap();
        assert_eq!(html, "<b>0</b><b>1</b>");
        assert_eq!(
            render("<b @for={n in 1..=3}>{{ n }}</b>"),
            "<b>1</b><b>2</b><b>3</b>"
        );
    }

    #[test]
    fn if_inside_for_is_checked_per_item() {
        let html = render_with(
            "<li @for={t in todos} @if={!t.done}>{{ t.title }}</li>",
            |scope| {
                scope.push_dynamic("todos", todos());
            },
        )
        .unwrap();
        assert_eq!(html, "<li>Зробити домашку</li>");
    }

    #[test]
    fn template_groups_nodes_without_a_wrapper() {
        let html = render_with(
            "<template @if={show}><b>a</b><i>b</i></template>",
            |scope| {
                scope.push("show", true);
            },
        )
        .unwrap();
        assert_eq!(html, "<template><b>a</b><i>b</i></template>");
    }

    #[test]
    fn class_directive_merges_with_static_class() {
        let html = render_with(
            "<li class=\"todo\" @class={#{\"completed\": done, \"urgent\": false}}>x</li>",
            |scope| {
                scope.push("done", true);
            },
        )
        .unwrap();
        assert_eq!(html, "<li class=\"todo completed\">x</li>");
    }

    #[test]
    fn attr_directive_drops_false_and_unit() {
        let html = render_with(
            "<input @attr={#{\"checked\": done, \"disabled\": false, \"value\": title}}>",
            |scope| {
                scope.push("done", true);
                scope.push("title", "Молоко");
            },
        )
        .unwrap();
        assert_eq!(html, "<input checked value=\"Молоко\">");
    }

    #[test]
    fn style_and_text_and_html_directives() {
        let html = render_with(
            "<div @style={#{\"width\": w}}></div><p @text={t}></p><p @html={h}></p>",
            |scope| {
                scope.push("w", "50%");
                scope.push("t", "<b>");
                scope.push("h", "<b>ок</b>");
            },
        )
        .unwrap();
        assert_eq!(
            html,
            "<div style=\"width:50%;\"></div><p>&lt;b&gt;</p><p><b>ок</b></p>"
        );
    }

    #[test]
    fn oob_directive_emits_htmx_attributes() {
        let html = render_with("<tr @oob={\"#row-\" + id}><td>x</td></tr>", |scope| {
            scope.push("id", 42_i64);
        })
        .unwrap();
        assert_eq!(
            html,
            "<tr id=\"row-42\" hx-swap-oob=\"outerHTML:#row-42\"><td>x</td></tr>"
        );
    }

    // -------------------------------------------------------- атрибути

    #[test]
    fn attributes_interpolate_and_escape() {
        let html = render_with("<a href=\"/todo/{{ id }}?q={{ q }}\">x</a>", |scope| {
            scope.push("id", 7_i64);
            scope.push("q", "a\"b");
        })
        .unwrap();
        assert_eq!(html, "<a href=\"/todo/7?q=a&quot;b\">x</a>");
    }

    #[test]
    fn expression_attributes_follow_value_rules() {
        let html = render_with(
            "<input required disabled={locked} value={title} data-x={missing}>",
            |scope| {
                scope.push("locked", true);
                scope.push("title", "Назва");
                scope.push("missing", ());
            },
        )
        .unwrap();
        assert_eq!(html, "<input required disabled value=\"Назва\">");
    }

    #[test]
    fn spread_expands_a_map() {
        let html = render_with("<input {...field}>", |scope| {
            let mut map = rhai::Map::new();
            map.insert("name".into(), Dynamic::from("title"));
            map.insert("required".into(), Dynamic::from(true));
            scope.push_dynamic("field", Dynamic::from_map(map));
        })
        .unwrap();
        assert_eq!(html, "<input name=\"title\" required>");
    }

    #[test]
    fn spread_drops_event_handlers_with_a_warning() {
        let engine = engine(Limits::default());
        let text = "<div {...attrs}></div>";
        let source = Arc::new(Source::new("test.rhx", text));
        let template = Template::compile(source, &engine).unwrap();
        let mut scope = Scope::new();
        let mut map = rhai::Map::new();
        map.insert("onclick".into(), Dynamic::from("alert(1)"));
        map.insert("title".into(), Dynamic::from("ок"));
        scope.push_dynamic("attrs", Dynamic::from_map(map));

        let rendered = template
            .render(&engine, &mut scope, Slots::default(), &Globals::default())
            .unwrap();
        assert_eq!(rendered.html, "<div title=\"ок\"></div>");
        assert_eq!(rendered.warnings.len(), 1);
        assert!(
            rendered.warnings[0].contains("onclick"),
            "{:?}",
            rendered.warnings
        );
    }

    // ---------------------------------------------------------- безпека

    #[test]
    fn javascript_urls_are_neutralised() {
        let html = render_with("<a href={link}>x</a>", |scope| {
            scope.push("link", "javascript:alert(1)");
        })
        .unwrap();
        assert_eq!(html, "<a href=\"#\">x</a>");
    }

    #[test]
    fn event_attributes_reject_expressions() {
        let message = error("<button onclick={code}>x</button>");
        assert!(
            message.contains("не може бути результатом виразу"),
            "{message}"
        );
    }

    #[test]
    fn script_allows_only_json() {
        let message = error("<script>const id = {{ todo.id }};</script>");
        assert!(message.contains("json"), "{message}");

        // Скрипт сторінки піднімається (M7), тому перевіряємо піднятий асет.
        let engine = engine(Limits::default());
        let source = Arc::new(Source::new(
            "test.rhx",
            "<script>const t = {{ json(todo) }};</script>",
        ));
        let template = Template::compile(source, &engine).unwrap();
        let mut scope = Scope::new();
        let mut map = rhai::Map::new();
        map.insert("title".into(), Dynamic::from("</script>"));
        scope.push_dynamic("todo", Dynamic::from_map(map));

        let rendered = template
            .render(&engine, &mut scope, Slots::default(), &Globals::default())
            .unwrap();
        let body = &rendered.scripts[0].body;
        assert!(!body.contains("</script>"), "{body}");
        assert!(body.contains("\\u003C"), "{body}");
    }

    #[test]
    fn style_forbids_interpolation() {
        let message = error("<style>.a { width: {{ w }} }</style>");
        assert!(message.contains("заборонена"), "{message}");
    }

    // --------------------------------------------------------- помилки

    #[test]
    fn unclosed_tag_points_at_the_opening() {
        let message = error("<div>\n  <p>текст\n</div>");
        assert!(
            message.contains("не закрито") || message.contains("очікувався"),
            "{message}"
        );
        assert!(message.contains("test.rhx:"), "{message}");
    }

    #[test]
    fn unknown_directive_lists_the_known_ones() {
        let message = error("<p @iff={x}>a</p>");
        assert!(message.contains("невідома директива"), "{message}");
        assert!(message.contains("@if"), "{message}");
    }

    #[test]
    fn orphan_else_is_reported() {
        let message = error("<p @else>a</p>");
        assert!(message.contains("`@else` без `@if`"), "{message}");
    }

    #[test]
    fn unterminated_interpolation_is_reported() {
        let message = error("<p>{{ name </p>");
        assert!(message.contains("не закрито"), "{message}");
    }

    #[test]
    fn bad_for_header_is_reported() {
        let message = error("<li @for={todos}>x</li>");
        assert!(message.contains("`in`"), "{message}");
    }

    #[test]
    fn missing_variable_points_into_the_file() {
        let message = error("<p>\n  <span>{{ todoz }}</span>\n</p>");
        assert!(message.contains("test.rhx:2:"), "{message}");
        assert!(message.contains("todoz"), "{message}");
    }

    // ------------------------------------------------------- компоненти

    /// Компоненти в пам'яті: тести не мають залежати від файлової системи.
    struct TestComponents {
        engine: Arc<rhai::Engine>,
        sources: std::collections::BTreeMap<String, String>,
        cache: std::sync::Mutex<std::collections::BTreeMap<String, Arc<Template>>>,
    }

    impl TestComponents {
        fn new(engine: Arc<rhai::Engine>, files: &[(&str, &str)]) -> Self {
            Self {
                engine,
                sources: files
                    .iter()
                    .map(|(name, text)| ((*name).to_owned(), (*text).to_owned()))
                    .collect(),
                cache: std::sync::Mutex::new(Default::default()),
            }
        }
    }

    impl Components for TestComponents {
        fn resolve(&self, name: &str, span: Span) -> Result<Arc<Template>, Diagnostic> {
            if let Some(found) = self.cache.lock().unwrap().get(name) {
                return Ok(found.clone());
            }
            let text = self.sources.get(name).ok_or_else(|| {
                Diagnostic::new(format!("компонент `<{name}>` не знайдено"), span)
            })?;
            let source = Arc::new(Source::new(format!("components/{name}.rhx"), text.clone()));
            let template = Arc::new(Template::compile_with(source, &self.engine, self)?);
            self.cache
                .lock()
                .unwrap()
                .insert(name.to_owned(), template.clone());
            Ok(template)
        }
    }

    fn render_app(
        page: &str,
        components: &[(&str, &str)],
        fill: impl FnOnce(&mut Scope),
    ) -> Result<String, String> {
        let engine = Arc::new(engine(Limits::default()));
        let registry = TestComponents::new(engine.clone(), components);
        let source = Arc::new(Source::new("pages/page.rhx", page));
        let template = Template::compile_with(source.clone(), &engine, &registry)
            .map_err(|d| d.render(&source))?;

        let mut scope = Scope::new();
        fill(&mut scope);
        let _ = template
            .run_script(&engine, &mut scope)
            .map_err(|d| template.describe(&d))?;
        template
            .render(&engine, &mut scope, Slots::default(), &Globals::default())
            .map(|rendered| rendered.html)
            .map_err(|d| template.describe(&d))
    }

    #[test]
    fn component_receives_props() {
        let html = render_app(
            "<ul><TodoItem @for={t in todos} todo={t} editable /></ul>",
            &[(
                "TodoItem",
                "---\nlet todo = props.todo;\nlet editable = props.editable ?? false;\n---\n<li @class={#{\"done\": todo.done, \"editable\": editable}}>{{ todo.title }}</li>",
            )],
            |scope| {
                scope.push_dynamic("todos", todos());
            },
        )
        .unwrap();
        assert_eq!(
            html,
            "<ul><li class=\"done editable\">Купити молоко</li>\
             <li class=\"editable\">Зробити домашку</li></ul>"
        );
    }

    #[test]
    fn props_are_visible_as_variables_too() {
        let html = render_app(
            "<Badge text=\"новий\" count={3} />",
            &[("Badge", "<b>{{ text }}: {{ count }}</b>")],
            |_| {},
        )
        .unwrap();
        assert_eq!(html, "<b>новий: 3</b>");
    }

    #[test]
    fn component_cannot_see_parent_variables() {
        // Ізоляція scope (SYNTAX 5.3): компонент бачить лише props і глобальні
        // об'єкти. Інакше він перестав би бути переносимим.
        let message = render_app("<Leak />", &[("Leak", "<b>{{ secret }}</b>")], |scope| {
            scope.push("secret", "таємниця");
        })
        .unwrap_err();
        assert!(message.contains("невідома змінна `secret`"), "{message}");
        assert!(message.contains("components/Leak.rhx"), "{message}");
    }

    #[test]
    fn spread_fills_props() {
        let html = render_app(
            "<Badge {...data} />",
            &[("Badge", "<b>{{ props.text }}/{{ props.count }}</b>")],
            |scope| {
                let mut map = rhai::Map::new();
                map.insert("text".into(), Dynamic::from("з мапи"));
                map.insert("count".into(), Dynamic::from(7_i64));
                scope.push_dynamic("data", Dynamic::from_map(map));
            },
        )
        .unwrap();
        assert_eq!(html, "<b>з мапи/7</b>");
    }

    #[test]
    fn slots_default_named_and_fallback() {
        let card = "<div class=\"card\">\
                    <header><slot name=\"header\">без назви</slot></header>\
                    <div class=\"body\"><slot /></div>\
                    <footer @if={slots.has(\"footer\")}><slot name=\"footer\" /></footer>\
                    </div>";

        let with_all = render_app(
            "<Card><template slot=\"header\"><h2>Заголовок</h2></template>Вміст<button slot=\"footer\">OK</button></Card>",
            &[("Card", card)],
            |_| {},
        )
        .unwrap();
        assert!(
            with_all.contains("<header><h2>Заголовок</h2></header>"),
            "{with_all}"
        );
        assert!(
            with_all.contains("<div class=\"body\">Вміст</div>"),
            "{with_all}"
        );
        assert!(
            with_all.contains("<footer><button>OK</button></footer>"),
            "{with_all}"
        );

        let bare = render_app("<Card>тільки вміст</Card>", &[("Card", card)], |_| {}).unwrap();
        assert!(bare.contains("<header>без назви</header>"), "{bare}");
        assert!(
            !bare.contains("<footer>"),
            "порожній слот ховає footer: {bare}"
        );
    }

    #[test]
    fn slot_content_uses_the_parent_scope() {
        let html = render_app(
            "<Card>{{ title }}</Card>",
            &[("Card", "<div><slot /></div>")],
            |scope| {
                scope.push("title", "зі сторінки");
            },
        )
        .unwrap();
        assert_eq!(html, "<div>зі сторінки</div>");
    }

    #[test]
    fn nested_components_work() {
        let html = render_app(
            "<TodoList todos={todos} />",
            &[
                (
                    "TodoList",
                    "<ul><TodoItem @for={t in props.todos} todo={t} /></ul>",
                ),
                ("TodoItem", "<li>{{ props.todo.title }}</li>"),
            ],
            |scope| {
                scope.push_dynamic("todos", todos());
            },
        )
        .unwrap();
        assert_eq!(
            html,
            "<ul><li>Купити молоко</li><li>Зробити домашку</li></ul>"
        );
    }

    #[test]
    fn component_assets_are_hoisted_and_deduplicated() {
        let engine = Arc::new(engine(Limits::default()));
        let registry = TestComponents::new(
            engine.clone(),
            &[(
                "Chip",
                concat!(
                    "<b>{{ props.text }}</b>",
                    "<style>.chip { color: red }</style>",
                    "<script>console.log(\"chip\");</script>"
                ),
            )],
        );
        let source = Arc::new(Source::new(
            "pages/page.rhx",
            "<Chip text=\"a\" /><Chip text=\"b\" />",
        ));
        let template = Template::compile_with(source, &engine, &registry).unwrap();

        let mut scope = Scope::new();
        let rendered = template
            .render(&engine, &mut scope, Slots::default(), &Globals::default())
            .unwrap();

        // Компонент ужито двічі — асет один.
        assert_eq!(rendered.styles.len(), 1);
        assert_eq!(rendered.scripts.len(), 1);
        assert!(
            rendered.styles[0].body.contains(".chip"),
            "{:?}",
            rendered.styles
        );
        // З розмітки вони зникли: їх ставить ядро, а не вміст сторінки.
        assert_eq!(rendered.html, "<b>a</b><b>b</b>");
    }

    #[test]
    fn layout_keeps_its_own_tags_in_place() {
        let engine = engine(Limits::default());
        let source = Arc::new(Source::new(
            "layouts/main.rhx",
            "<head><style>body{margin:0}</style></head><body><slot /></body>",
        ));
        let template = Template::compile(source, &engine).unwrap();

        let mut scope = Scope::new();
        let rendered = template
            .render_with(
                &engine,
                &mut scope,
                Slots {
                    slot: "<p>сторінка</p>",
                    ..Slots::default()
                },
                &Globals::default(),
                false,
            )
            .unwrap();

        assert!(rendered.styles.is_empty(), "layout нічого не піднімає");
        assert!(
            rendered.html.contains("<style>body{margin:0}</style>"),
            "{}",
            rendered.html
        );
    }

    #[test]
    fn script_with_src_stays_where_it_is() {
        let engine = engine(Limits::default());
        let source = Arc::new(Source::new(
            "pages/page.rhx",
            "<p>a</p><script src=\"/app.js\"></script>",
        ));
        let template = Template::compile(source, &engine).unwrap();
        let mut scope = Scope::new();
        let rendered = template
            .render(&engine, &mut scope, Slots::default(), &Globals::default())
            .unwrap();

        assert!(rendered.scripts.is_empty(), "тег із src не піднімається");
        assert!(
            rendered.html.contains("src=\"/app.js\""),
            "{}",
            rendered.html
        );
    }

    #[test]
    fn unknown_component_is_a_compile_error() {
        let message = render_app("<Missing />", &[], |_| {}).unwrap_err();
        assert!(message.contains("не знайдено"), "{message}");
        assert!(message.contains("pages/page.rhx:1:"), "{message}");
    }

    #[test]
    fn error_inside_a_component_names_the_component_file() {
        let message =
            render_app("<Broken />", &[("Broken", "<p>{{ 1 / oops }}</p>")], |_| {}).unwrap_err();
        assert!(message.contains("components/Broken.rhx:1:"), "{message}");
        assert!(message.contains("у ланцюжку"), "{message}");
    }

    // ------------------------------------------------------- frontmatter

    #[test]
    fn frontmatter_is_separated_and_not_rendered() {
        let engine = engine(Limits::default());
        let text = "---\nlet secret = 1;\n---\n<p>видно</p>";
        let source = Arc::new(Source::new("test.rhx", text));
        let template = Template::compile(source, &engine).unwrap();
        assert_eq!(template.frontmatter(), Some("let secret = 1;"));

        let mut scope = Scope::new();
        let rendered = template
            .render(&engine, &mut scope, Slots::default(), &Globals::default())
            .unwrap();
        assert_eq!(rendered.html, "<p>видно</p>");
    }

    #[test]
    fn frontmatter_defines_what_the_markup_renders() {
        let engine = engine(Limits::default());
        let text = concat!(
            "---
",
            "let titles = [\"перше\", \"друге\"];
",
            "let total = titles.len();
",
            "---
",
            "<p>{{ total }}</p><li @for={t in titles}>{{ t }}</li>"
        );
        let source = Arc::new(Source::new("test.rhx", text));
        let template = Template::compile(source, &engine).unwrap();

        let mut scope = Scope::new();
        let _ = template.run_script(&engine, &mut scope).unwrap();
        let rendered = template
            .render(&engine, &mut scope, Slots::default(), &Globals::default())
            .unwrap();
        assert_eq!(rendered.html, "<p>2</p><li>перше</li><li>друге</li>");
    }

    #[test]
    fn frontmatter_error_points_into_the_file() {
        let engine = engine(Limits::default());
        let text = "---
let a = 1;
let b = missing_variable + 1;
---
<p>x</p>";
        let source = Arc::new(Source::new("test.rhx", text));
        let template = Template::compile(source, &engine).unwrap();

        let mut scope = Scope::new();
        let diagnostic = template.run_script(&engine, &mut scope).unwrap_err();
        let message = template.describe(&diagnostic);
        assert!(message.contains("test.rhx:3:"), "{message}");
        assert!(message.contains("missing_variable"), "{message}");
    }

    #[test]
    fn frontmatter_syntax_error_is_caught_at_compile_time() {
        let engine = engine(Limits::default());
        let text = "---
let a = ;
---
<p>x</p>";
        let source = Arc::new(Source::new("test.rhx", text));
        let diagnostic = Template::compile(source.clone(), &engine).unwrap_err();
        let message = diagnostic.render(&source);
        assert!(message.contains("test.rhx:2:"), "{message}");
    }

    #[test]
    fn returned_value_becomes_the_body() {
        let engine = engine(Limits::default());
        let text = "---
return \"<b>готово</b>\";
---
<p>це не рендериться</p>";
        let source = Arc::new(Source::new("test.rhx", text));
        let template = Template::compile(source, &engine).unwrap();

        let mut scope = Scope::new();
        let value = template.run_script(&engine, &mut scope).unwrap();
        assert!(!value.is_unit());
        assert_eq!(value.cast::<String>(), "<b>готово</b>");
    }

    #[test]
    fn layout_slots_are_filled_by_the_core() {
        let engine = engine(Limits::default());
        let text = "<head><rhaix:head /></head><body><main><slot /></main><rhaix:scripts /></body>";
        let source = Arc::new(Source::new("layout.rhx", text));
        let template = Template::compile(source, &engine).unwrap();
        let mut scope = Scope::new();
        let rendered = template
            .render(
                &engine,
                &mut scope,
                Slots {
                    slot: "<h1>сторінка</h1>",
                    head: "<link rel=\"stylesheet\" href=\"/s.css\">",
                    scripts: "<script src=\"/app.js\"></script>",
                },
                &Globals::default(),
            )
            .unwrap();
        assert_eq!(
            rendered.html,
            "<head><link rel=\"stylesheet\" href=\"/s.css\"></head>\
             <body><main><h1>сторінка</h1></main><script src=\"/app.js\"></script></body>"
        );
    }
}
