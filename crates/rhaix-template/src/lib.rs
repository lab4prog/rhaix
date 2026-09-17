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
mod parse;
mod render;

use std::sync::Arc;

use rhai::{Engine, Scope};
use rhaix_parser::{Source, Span};

pub use ast::Node;
pub use error::Diagnostic;
pub use render::{Rendered, Slots};

/// Скомпільований шаблон. Спільний для всіх запитів: на запит змінюється
/// тільки `Scope`.
#[derive(Debug, Clone)]
pub struct Template {
    source: Arc<Source>,
    nodes: Vec<Node>,
    frontmatter: Option<Span>,
}

impl Template {
    pub fn compile(source: Arc<Source>, engine: &Engine) -> Result<Self, Diagnostic> {
        let split = rhaix_parser::split(&source).map_err(|err| {
            let diagnostic = Diagnostic::new(err.message, err.span);
            match err.hint {
                Some(hint) => diagnostic.with_hint(hint),
                None => diagnostic,
            }
        })?;
        let nodes = parse::parse(&source, engine, split.markup)?;
        Ok(Self {
            source,
            nodes,
            frontmatter: split.frontmatter,
        })
    }

    /// Rhai-код frontmatter. Виконання з'явиться в M2.
    pub fn frontmatter(&self) -> Option<&str> {
        self.frontmatter.map(|span| self.source.slice(span))
    }

    pub fn source(&self) -> &Source {
        &self.source
    }

    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    pub fn render(
        &self,
        engine: &Engine,
        scope: &mut Scope,
        slots: Slots<'_>,
    ) -> Result<Rendered, Diagnostic> {
        render::render(&self.source, engine, scope, &self.nodes, slots)
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
            .render(&engine, &mut scope, Slots::default())
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
            .render(&engine, &mut scope, Slots::default())
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

        let html = render_with("<script>const t = {{ json(todo) }};</script>", |scope| {
            let mut map = rhai::Map::new();
            map.insert("title".into(), Dynamic::from("</script>"));
            scope.push_dynamic("todo", Dynamic::from_map(map));
        })
        .unwrap();
        assert!(!html.contains("</script><"), "{html}");
        assert!(html.contains("\\u003C"), "{html}");
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

    #[test]
    fn components_report_that_they_arrive_in_m3() {
        let message = error("<TodoItem todo={t} />");
        assert!(message.contains("M3"), "{message}");
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
            .render(&engine, &mut scope, Slots::default())
            .unwrap();
        assert_eq!(rendered.html, "<p>видно</p>");
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
            )
            .unwrap();
        assert_eq!(
            rendered.html,
            "<head><link rel=\"stylesheet\" href=\"/s.css\"></head>\
             <body><main><h1>сторінка</h1></main><script src=\"/app.js\"></script></body>"
        );
    }
}
