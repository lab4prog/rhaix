//! Розбір `.rhx`: текст, `{{ }}`, елементи, атрибути й директиви.
//!
//! Парсер навмисно строгий (RISKS 2.4): незакритий тег або невідома директива —
//! це помилка з підказкою, а не тихо з'їдена розмітка. Він не намагається бути
//! HTML5-парсером: його цікавлять лише теги, атрибути й директиви, а решта
//! тексту переноситься у вивід дослівно.

use rhai::Engine;
use rhaix_parser::{Source, Span};

use crate::ast::*;
use crate::error::{Diagnostic, Result};
use crate::escape::{is_event_attribute, is_url_attribute, Context};
use crate::expr::Expr;
use crate::loader::Components;

const DIRECTIVES: [&str; 11] = [
    "if", "else-if", "else", "for", "key", "class", "style", "attr", "html", "text", "oob",
];

pub fn parse(
    source: &Source,
    engine: &Engine,
    components: &dyn Components,
    markup: Span,
) -> Result<Vec<Node>> {
    let mut parser = Parser {
        source,
        engine,
        components,
        pos: markup.start,
        end: markup.end,
        dropped: 0,
    };
    let items = parser.parse_items(None)?;
    if parser.pos < parser.end {
        let span = Span::new(parser.pos, (parser.pos + 2).min(parser.end));
        return Err(Diagnostic::new("закривальний тег без відкривального", span));
    }
    finish(source, items)
}

struct Parser<'a> {
    source: &'a Source,
    engine: &'a Engine,
    components: &'a dyn Components,
    pos: usize,
    end: usize,
    /// Скільки коментарів викинуто. Якщо всередині елемента лічильник
    /// змінився, його вивід уже не збігається з джерелом байт у байт.
    dropped: usize,
}

/// Вузол разом із директивами потоку, які ще не застосовані.
struct Item {
    node: Node,
    flow: Flow,
    span: Span,
}

#[derive(Default)]
struct Flow {
    if_: Option<Expr>,
    else_if: Option<Expr>,
    else_: Option<Span>,
    for_: Option<ForHeader>,
    key: Option<Expr>,
}

impl Flow {
    fn is_empty(&self) -> bool {
        self.if_.is_none()
            && self.else_if.is_none()
            && self.else_.is_none()
            && self.for_.is_none()
            && self.key.is_none()
    }
}

struct ForHeader {
    item: String,
    index: Option<String>,
    list: Expr,
}

impl<'a> Parser<'a> {
    fn rest(&self) -> &'a str {
        &self.source.text()[self.pos..self.end]
    }

    fn at(&self, text: &str) -> bool {
        self.rest().starts_with(text)
    }

    fn span_here(&self, len: usize) -> Span {
        Span::new(self.pos, (self.pos + len).min(self.end))
    }

    fn skip_whitespace(&mut self) {
        while let Some(ch) = self.rest().chars().next() {
            if ch.is_whitespace() {
                self.pos += ch.len_utf8();
            } else {
                break;
            }
        }
    }

    // ------------------------------------------------------------- вузли

    fn parse_items(&mut self, stop: Option<&str>) -> Result<Vec<Item>> {
        let mut items = Vec::new();
        while self.pos < self.end {
            if self.at("</") {
                // закривальний тег розбирає той, хто відкривав
                break;
            }
            if self.at("{{") {
                if let Some(item) = self.parse_interp(Context::Text)? {
                    items.push(item);
                }
                continue;
            }
            if self.at("<!--") {
                let start = self.pos;
                let end = self
                    .rest()
                    .find("-->")
                    .map(|i| self.pos + i + 3)
                    .unwrap_or(self.end);
                self.pos = end;
                items.push(self.plain(Node::Text(Span::new(start, end)), Span::new(start, end)));
                continue;
            }
            if self.at("<") && self.tag_starts_here() {
                items.push(self.parse_tag()?);
                continue;
            }
            items.push(self.parse_text());
        }
        let _ = stop;
        Ok(items)
    }

    /// `<` починає тег лише якщо далі йде буква — інакше це звичайний текст
    /// (наприклад, `a < b` у вмісті сторінки).
    fn tag_starts_here(&self) -> bool {
        self.rest()
            .chars()
            .nth(1)
            .map(|ch| ch.is_ascii_alphabetic())
            .unwrap_or(false)
    }

    fn parse_text(&mut self) -> Item {
        let start = self.pos;
        // перший символ уже не є початком тега, тому пропускаємо саме його
        // (а не один байт: на кирилиці це розрізало б символ)
        let first = self
            .rest()
            .chars()
            .next()
            .map(|ch| ch.len_utf8())
            .unwrap_or(1);
        let mut cursor = self.pos + first;
        let text = self.source.text();
        while cursor < self.end {
            let rest = &text[cursor..self.end];
            if rest.starts_with("{{") || rest.starts_with("</") || rest.starts_with("<!--") {
                break;
            }
            if rest.starts_with('<')
                && rest
                    .chars()
                    .nth(1)
                    .map(|c| c.is_ascii_alphabetic())
                    .unwrap_or(false)
            {
                break;
            }
            cursor += rest.chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        }
        self.pos = cursor.min(self.end);
        let span = Span::new(start, self.pos);
        self.plain(Node::Text(span), span)
    }

    /// `{{ вираз }}`, `{{! коментар }}`, з можливими маркерами обрізання.
    fn parse_interp(&mut self, context: Context) -> Result<Option<Item>> {
        let open = self.pos;
        let body_start = open + 2;
        let text = self.source.text();

        if text[body_start..self.end].starts_with('!') {
            let close = text[body_start..self.end].find("}}").ok_or_else(|| {
                Diagnostic::new("коментар `{{!` не закрито", self.span_here(3))
                    .with_hint("закрийте його через `}}`")
            })?;
            self.pos = body_start + close + 2;
            self.dropped += 1;
            return Ok(None);
        }

        let close_rel = text[body_start..self.end].find("}}").ok_or_else(|| {
            Diagnostic::new("інтерполяцію `{{` не закрито", self.span_here(2))
                .with_hint("закрийте її через `}}`")
        })?;
        let close = body_start + close_rel;
        self.pos = close + 2;

        let mut start = body_start;
        let mut end = close;
        let trim_left = text[start..end].starts_with('-');
        if trim_left {
            start += 1;
        }
        let trim_right = end > start && text[start..end].ends_with('-');
        if trim_right {
            end -= 1;
        }

        let span = Span::new(start, end);
        if context == Context::Script {
            let body = text[start..end].trim_start();
            if !(body.starts_with("json(") || body.starts_with("raw(")) {
                return Err(
                    Diagnostic::new("у `<script>` дозволено лише `json(...)`", span).with_hint(
                        "HTML-екранування всередині JS не діє; напишіть `{{ json(значення) }}`",
                    ),
                );
            }
        }

        let expr = Expr::compile(self.engine, self.source, span)?;
        let node = Node::Interp(Interp {
            expr,
            context,
            trim_left,
            trim_right,
        });
        let full = Span::new(open, self.pos);
        Ok(Some(self.plain(node, full)))
    }

    // ------------------------------------------------------------- теги

    fn parse_tag(&mut self) -> Result<Item> {
        let open = self.pos;
        self.pos += 1;
        let name_start = self.pos;
        let name = self.read_tag_name();
        let name_span = Span::new(name_start, self.pos);
        if name.is_empty() {
            return Err(Diagnostic::new("очікувалось ім'я тега", name_span));
        }

        let parsed = self.parse_attributes(&name, name_span)?;
        let tag_end = self.pos;
        let dropped_before = self.dropped;

        // службові теги ядра
        if let Some(special) = self.special_kind(&name, Span::new(open, tag_end)) {
            if !parsed.empty {
                self.expect_close(&name, Span::new(open, tag_end))?;
            }
            return Ok(Item {
                node: Node::Special(special),
                flow: parsed.flow,
                span: Span::new(open, self.pos),
            });
        }

        if name == "slot" {
            let slot_name = self.slot_name(&parsed.attrs, name_span)?;
            let fallback = if parsed.empty {
                Vec::new()
            } else {
                let items = self.parse_items(Some(&name))?;
                self.expect_close(&name, Span::new(open, tag_end))?;
                finish(self.source, items)?
            };
            let span = Span::new(open, self.pos);
            return Ok(Item {
                node: Node::Slot(Box::new(SlotNode {
                    name: slot_name,
                    fallback,
                    span,
                })),
                flow: parsed.flow,
                span,
            });
        }

        if name == "rhaix:raw" {
            let content_start = self.pos;
            let close = self.rest().find("</rhaix:raw>").ok_or_else(|| {
                Diagnostic::new("`<rhaix:raw>` не закрито", Span::new(open, tag_end))
            })?;
            let content = Span::new(content_start, content_start + close);
            self.pos = content_start + close + "</rhaix:raw>".len();
            return Ok(self.plain(Node::Text(content), Span::new(open, self.pos)));
        }

        let is_component = name
            .chars()
            .next()
            .map(|ch| ch.is_ascii_uppercase())
            .unwrap_or(false);

        let empty = parsed.empty || is_void(&name);
        let children = if empty {
            Vec::new()
        } else if is_raw_text(&name) {
            self.parse_raw_text(&name, Span::new(open, tag_end))?
        } else {
            let items = self.parse_items(Some(&name))?;
            self.expect_close(&name, Span::new(open, tag_end))?;
            finish(self.source, items)?
        };

        let span = Span::new(open, self.pos);

        if is_component {
            // Компонент шукається вже зараз: невідомий тег і цикл — це помилка
            // компіляції, а не сюрприз під час запиту.
            let template = self.components.resolve(&name, name_span)?;
            let (children, named) = split_slots(self.source, children);
            return Ok(Item {
                node: Node::Component(Box::new(Component {
                    name,
                    attrs: parsed.attrs,
                    children,
                    named,
                    template,
                    span,
                })),
                flow: parsed.flow,
                span,
            });
        }

        // Якщо в піддереві немає нічого динамічного, його вивід дослівно
        // збігається зі шматком джерела — тоді весь елемент стає одним
        // текстовим вузлом, і рендер не обходить його взагалі.
        if !is_component
            && self.dropped == dropped_before
            && parsed.flow.is_empty()
            && parsed.bind.is_empty()
            && parsed.attrs.iter().all(attribute_is_static)
            // елемент, що позначає слот, має лишитись елементом
            && !parsed.attrs.iter().any(|attr| attr.name == "slot")
            && children.iter().all(|child| matches!(child, Node::Text(_)))
        {
            return Ok(self.plain(Node::Text(span), span));
        }

        Ok(Item {
            node: Node::Element(Box::new(Element {
                name,
                name_span,
                attrs: parsed.attrs,
                bind: parsed.bind,
                children,
                empty,
                span,
            })),
            flow: parsed.flow,
            span,
        })
    }

    fn special_kind(&self, name: &str, span: Span) -> Option<Special> {
        match name {
            "rhaix:head" => Some(Special::Head(span)),
            "rhaix:scripts" => Some(Special::Scripts(span)),
            _ => None,
        }
    }

    /// `<slot name="header">` — єдиний дозволений атрибут слота.
    fn slot_name(&self, attrs: &[Attribute], span: Span) -> Result<Option<String>> {
        let mut name = None;
        for attr in attrs {
            if attr.name != "name" {
                return Err(Diagnostic::new(
                    format!("`<slot>` не має атрибута `{}`", attr.name),
                    attr.span,
                )
                .with_hint("слот приймає лише `name`"));
            }
            match &attr.value {
                AttrValue::Parts(parts) => {
                    let mut text = String::new();
                    for part in parts {
                        match part {
                            AttrPart::Text(span) => text.push_str(self.source.slice(*span)),
                            AttrPart::Interp(expr) => {
                                return Err(Diagnostic::new(
                                    "ім'я слота має бути сталим",
                                    expr.span(),
                                ))
                            }
                        }
                    }
                    name = Some(text);
                }
                _ => {
                    return Err(Diagnostic::new("ім'я слота має бути рядком", span));
                }
            }
        }
        Ok(name)
    }

    /// Вміст `<script>`, `<style>`, `<pre>`, `<textarea>` розміткою не є.
    fn parse_raw_text(&mut self, name: &str, open_span: Span) -> Result<Vec<Node>> {
        let closing = format!("</{name}");
        let start = self.pos;
        let close_rel = self.rest().find(closing.as_str()).ok_or_else(|| {
            Diagnostic::new(format!("тег `<{name}>` не закрито"), open_span)
                .with_hint(format!("додайте `</{name}>`"))
        })?;
        let content_end = start + close_rel;
        let context = content_context(name);

        if name == "style" && self.source.text()[start..content_end].contains("{{") {
            let at = start + self.source.text()[start..content_end].find("{{").unwrap();
            return Err(Diagnostic::new(
                "інтерполяція всередині `<style>` заборонена",
                Span::new(at, at + 2),
            )
            .with_hint("динамічні значення передавайте через `@style` або CSS-змінні"));
        }

        // всередині тексту все одно можуть бути `{{ }}` — розбираємо їх окремо
        let saved_end = self.end;
        self.end = content_end;
        let items = self.parse_scan(context)?;
        self.end = saved_end;
        self.pos = content_end;

        self.expect_close(name, open_span)?;
        finish(self.source, items)
    }

    /// Прохід по тексту, де теги не розбираються: лише текст і `{{ }}`.
    fn parse_scan(&mut self, context: Context) -> Result<Vec<Item>> {
        let mut items = Vec::new();
        while self.pos < self.end {
            if self.at("{{") {
                if let Some(item) = self.parse_interp(context)? {
                    items.push(item);
                }
                continue;
            }
            let start = self.pos;
            let next = self
                .rest()
                .find("{{")
                .map(|i| start + i)
                .unwrap_or(self.end);
            self.pos = next;
            let span = Span::new(start, next);
            items.push(self.plain(Node::Text(span), span));
        }
        Ok(items)
    }

    fn expect_close(&mut self, name: &str, open_span: Span) -> Result<()> {
        if !self.at("</") {
            return Err(
                Diagnostic::new(format!("тег `<{name}>` не закрито"), open_span)
                    .with_hint(format!("додайте `</{name}>`")),
            );
        }
        let start = self.pos;
        self.pos += 2;
        let closing = self.read_tag_name();
        if closing != name {
            let span = Span::new(start, self.pos);
            return Err(Diagnostic::new(
                format!("очікувався `</{name}>`, а тут `</{closing}>`"),
                span,
            )
            .with_hint("перевірте порядок вкладення тегів"));
        }
        self.skip_whitespace();
        if !self.at(">") {
            return Err(Diagnostic::new(
                format!("закривальний тег `</{name}` не завершено"),
                self.span_here(1),
            ));
        }
        self.pos += 1;
        Ok(())
    }

    fn read_tag_name(&mut self) -> String {
        let start = self.pos;
        while let Some(ch) = self.rest().chars().next() {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | ':' | '.') {
                self.pos += ch.len_utf8();
            } else {
                break;
            }
        }
        self.source.text()[start..self.pos].to_owned()
    }

    // -------------------------------------------------------- атрибути

    fn parse_attributes(&mut self, tag: &str, tag_span: Span) -> Result<ParsedTag> {
        let mut attrs = Vec::new();
        let mut bind = Bindings::default();
        let mut flow = Flow::default();

        loop {
            self.skip_whitespace();
            if self.pos >= self.end {
                return Err(
                    Diagnostic::new(format!("тег `<{tag}` не завершено"), tag_span)
                        .with_hint("бракує `>`"),
                );
            }
            if self.at("/>") {
                self.pos += 2;
                return Ok(ParsedTag {
                    attrs,
                    bind,
                    flow,
                    empty: true,
                });
            }
            if self.at(">") {
                self.pos += 1;
                return Ok(ParsedTag {
                    attrs,
                    bind,
                    flow,
                    empty: false,
                });
            }

            if self.at("{") {
                let span = self.read_braced()?;
                let inner = self.source.slice(span);
                let trimmed = inner.trim_start();
                if !trimmed.starts_with("...") {
                    return Err(Diagnostic::new("очікувалось `{...вираз}`", span).with_hint(
                        "у фігурних дужках без імені атрибута може бути лише розпакування мапи",
                    ));
                }
                let offset = inner.len() - trimmed.len() + 3;
                let expr_span = Span::new(span.start + offset, span.end);
                attrs.push(Attribute {
                    name: String::new(),
                    span,
                    value: AttrValue::Spread(Expr::compile(self.engine, self.source, expr_span)?),
                });
                continue;
            }

            let name_start = self.pos;
            let name = self.read_attr_name();
            let name_span = Span::new(name_start, self.pos);
            if name.is_empty() {
                return Err(Diagnostic::new(
                    "очікувалось ім'я атрибута",
                    self.span_here(1),
                ));
            }

            if let Some(directive) = name.strip_prefix('@') {
                self.parse_directive(directive, name_span, &mut bind, &mut flow)?;
                continue;
            }

            self.skip_whitespace();
            if !self.at("=") {
                attrs.push(Attribute {
                    name,
                    span: name_span,
                    value: AttrValue::Boolean,
                });
                continue;
            }
            self.pos += 1;
            self.skip_whitespace();

            let value = self.parse_attr_value(&name)?;
            if is_event_attribute(&name) && value_is_dynamic(&value) {
                return Err(Diagnostic::new(
                    format!("`{name}` не може бути результатом виразу"),
                    name_span,
                )
                .with_hint("обробники подій пишуться статично; для динаміки використайте htmx"));
            }
            attrs.push(Attribute {
                name,
                span: name_span,
                value,
            });
        }
    }

    fn read_attr_name(&mut self) -> String {
        let start = self.pos;
        while let Some(ch) = self.rest().chars().next() {
            if ch.is_whitespace() || matches!(ch, '=' | '>' | '/' | '"' | '\'') {
                break;
            }
            self.pos += ch.len_utf8();
        }
        self.source.text()[start..self.pos].to_owned()
    }

    fn parse_attr_value(&mut self, name: &str) -> Result<AttrValue> {
        let context = if is_url_attribute(name) {
            Context::Url
        } else {
            Context::Attribute
        };

        if self.at("{") {
            let span = self.read_braced()?;
            return Ok(AttrValue::Expr(Expr::compile(
                self.engine,
                self.source,
                span,
            )?));
        }

        let quote = self.rest().chars().next().unwrap_or(' ');
        if quote == '"' || quote == '\'' {
            self.pos += 1;
            let parts = self.parse_attr_parts(quote, context)?;
            return Ok(AttrValue::Parts(parts));
        }

        let start = self.pos;
        while let Some(ch) = self.rest().chars().next() {
            if ch.is_whitespace() || ch == '>' {
                break;
            }
            self.pos += ch.len_utf8();
        }
        Ok(AttrValue::Parts(vec![AttrPart::Text(Span::new(
            start, self.pos,
        ))]))
    }

    fn parse_attr_parts(&mut self, quote: char, context: Context) -> Result<Vec<AttrPart>> {
        let mut parts = Vec::new();
        loop {
            if self.pos >= self.end {
                return Err(Diagnostic::new(
                    "значення атрибута не закрито",
                    self.span_here(1),
                ));
            }
            if self.rest().starts_with(quote) {
                self.pos += 1;
                return Ok(parts);
            }
            if self.at("{{") {
                let before = self.pos;
                if let Some(item) = self.parse_interp(context)? {
                    match item.node {
                        Node::Interp(interp) => parts.push(AttrPart::Interp(interp.expr)),
                        _ => unreachable!("parse_interp повертає лише Interp"),
                    }
                } else {
                    let _ = before;
                }
                continue;
            }
            let start = self.pos;
            let mut cursor = self.pos;
            while cursor < self.end {
                let rest = &self.source.text()[cursor..self.end];
                if rest.starts_with(quote) || rest.starts_with("{{") {
                    break;
                }
                cursor += rest.chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            }
            self.pos = cursor;
            parts.push(AttrPart::Text(Span::new(start, cursor)));
        }
    }

    /// Прочитати `{ ... }` з урахуванням вкладених дужок і рядків.
    fn read_braced(&mut self) -> Result<Span> {
        let open = self.pos;
        self.pos += 1;
        let start = self.pos;
        let text = self.source.text();
        let mut depth = 1usize;
        let mut quote: Option<char> = None;
        let mut escaped = false;

        while self.pos < self.end {
            let ch = text[self.pos..].chars().next().unwrap();
            let len = ch.len_utf8();
            if let Some(active) = quote {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == active {
                    quote = None;
                }
            } else {
                match ch {
                    '"' | '\'' | '`' => quote = Some(ch),
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            let span = Span::new(start, self.pos);
                            self.pos += len;
                            return Ok(span);
                        }
                    }
                    _ => {}
                }
            }
            self.pos += len;
        }

        Err(
            Diagnostic::new("не знайдено закривальну `}`", Span::new(open, open + 1))
                .with_hint("перевірте дужки у виразі"),
        )
    }

    // ------------------------------------------------------- директиви

    fn parse_directive(
        &mut self,
        name: &str,
        name_span: Span,
        bind: &mut Bindings,
        flow: &mut Flow,
    ) -> Result<()> {
        if !DIRECTIVES.contains(&name) {
            return Err(
                Diagnostic::new(format!("невідома директива `@{name}`"), name_span).with_hint(
                    format!(
                        "доступні: {}",
                        DIRECTIVES.map(|d| format!("@{d}")).join(", ")
                    ),
                ),
            );
        }

        self.skip_whitespace();
        let value = if self.at("=") {
            self.pos += 1;
            self.skip_whitespace();
            if !self.at("{") {
                return Err(Diagnostic::new(
                    format!("значення `@{name}` пишеться у фігурних дужках"),
                    self.span_here(1),
                )
                .with_hint(format!("наприклад, `@{name}={{вираз}}`")));
            }
            Some(self.read_braced()?)
        } else {
            None
        };

        let require = |span: Option<Span>| -> Result<Span> {
            span.ok_or_else(|| {
                Diagnostic::new(format!("`@{name}` вимагає значення"), name_span)
                    .with_hint(format!("напишіть `@{name}={{вираз}}`"))
            })
        };

        match name {
            "if" => flow.if_ = Some(self.compile(require(value)?)?),
            "else-if" => flow.else_if = Some(self.compile(require(value)?)?),
            "else" => flow.else_ = Some(name_span),
            "key" => flow.key = Some(self.compile(require(value)?)?),
            "for" => flow.for_ = Some(self.parse_for_header(require(value)?)?),
            "class" => bind.class = Some(self.compile(require(value)?)?),
            "style" => bind.style = Some(self.compile(require(value)?)?),
            "attr" => bind.attr = Some(self.compile(require(value)?)?),
            "html" => bind.html = Some(self.compile(require(value)?)?),
            "text" => bind.text = Some(self.compile(require(value)?)?),
            "oob" => bind.oob = Some(self.compile(require(value)?)?),
            _ => unreachable!("список директив перевірено вище"),
        }
        Ok(())
    }

    fn compile(&self, span: Span) -> Result<Expr> {
        Expr::compile(self.engine, self.source, span)
    }

    /// `t in todos`, `(t, i) in todos`
    fn parse_for_header(&self, span: Span) -> Result<ForHeader> {
        let text = self.source.slice(span);
        let keyword = find_in_keyword(text).ok_or_else(|| {
            Diagnostic::new("у `@for` бракує `in`", span)
                .with_hint("формат: `@for={елемент in колекція}`")
        })?;

        let pattern = text[..keyword].trim();
        let list_start = keyword + 2;
        let list_text = &text[list_start..];
        let lead = list_text.len() - list_text.trim_start().len();
        let list_span = Span::new(
            span.start + list_start + lead,
            span.start + list_start + list_text.trim_end().len(),
        );
        if list_span.is_empty() {
            return Err(Diagnostic::new("у `@for` бракує колекції", span));
        }

        let (item, index) = parse_for_pattern(pattern).ok_or_else(|| {
            Diagnostic::new(format!("незрозумілий елемент циклу: `{pattern}`"), span)
                .with_hint("формат: `t in todos` або `(t, i) in todos`")
        })?;

        Ok(ForHeader {
            item,
            index,
            list: self.compile(list_span)?,
        })
    }

    fn plain(&self, node: Node, span: Span) -> Item {
        Item {
            node,
            flow: Flow::default(),
            span,
        }
    }
}

/// Розкласти вміст компонента на слот за замовчуванням і іменовані.
///
/// `<template slot="header">…</template>` віддає лише свій вміст, будь-який
/// інший елемент із `slot="…"` потрапляє в слот цілком (без самого атрибута).
fn split_slots(source: &Source, children: Vec<Node>) -> (Vec<Node>, Vec<(String, Vec<Node>)>) {
    let mut default = Vec::new();
    let mut named: Vec<(String, Vec<Node>)> = Vec::new();

    for child in children {
        let Node::Element(element) = child else {
            default.push(child);
            continue;
        };

        let slot = element
            .attrs
            .iter()
            .position(|attr| attr.name == "slot")
            .and_then(|index| match &element.attrs[index].value {
                AttrValue::Parts(parts) => match parts.as_slice() {
                    [AttrPart::Text(span)] => Some((index, *span)),
                    _ => None,
                },
                _ => None,
            });

        let Some((index, span)) = slot else {
            default.push(Node::Element(element));
            continue;
        };

        let mut element = *element;
        element.attrs.remove(index);
        let name = source.slice(span).to_owned();
        let nodes = if element.name == "template" {
            std::mem::take(&mut element.children)
        } else {
            vec![Node::Element(Box::new(element))]
        };
        named.push((name, nodes));
    }

    (default, named)
}

/// Атрибут без жодного виразу — його можна віддати як частину тексту.
fn attribute_is_static(attr: &Attribute) -> bool {
    match &attr.value {
        AttrValue::Boolean => true,
        AttrValue::Parts(parts) => parts.iter().all(|p| matches!(p, AttrPart::Text(_))),
        AttrValue::Expr(_) | AttrValue::Spread(_) => false,
    }
}

struct ParsedTag {
    attrs: Vec<Attribute>,
    bind: Bindings,
    flow: Flow,
    empty: bool,
}

fn value_is_dynamic(value: &AttrValue) -> bool {
    match value {
        AttrValue::Expr(_) | AttrValue::Spread(_) => true,
        AttrValue::Parts(parts) => parts.iter().any(|p| matches!(p, AttrPart::Interp(_))),
        AttrValue::Boolean => false,
    }
}

/// Знайти `in` як окреме слово поза дужками й рядками.
fn find_in_keyword(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut quote: Option<u8> = None;
    let mut i = 0usize;
    while i < bytes.len() {
        let ch = bytes[i];
        if let Some(active) = quote {
            if ch == b'\\' {
                i += 2;
                continue;
            }
            if ch == active {
                quote = None;
            }
            i += 1;
            continue;
        }
        match ch {
            b'"' | b'\'' | b'`' => quote = Some(ch),
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'i' if depth == 0 && bytes.get(i + 1) == Some(&b'n') => {
                let before_ok =
                    i == 0 || bytes[i - 1].is_ascii_whitespace() || bytes[i - 1] == b')';
                let after_ok = bytes
                    .get(i + 2)
                    .map(|c| c.is_ascii_whitespace())
                    .unwrap_or(false);
                if before_ok && after_ok {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn parse_for_pattern(pattern: &str) -> Option<(String, Option<String>)> {
    let inner = pattern.trim();
    if let Some(stripped) = inner.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
        let mut parts = stripped.split(',').map(str::trim);
        let item = parts.next()?;
        let index = parts.next()?;
        if parts.next().is_some() || !is_name(item) || !is_name(index) {
            return None;
        }
        return Some((item.to_owned(), Some(index.to_owned())));
    }
    is_name(inner).then(|| (inner.to_owned(), None))
}

fn is_name(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .next()
            .map(|c| c.is_alphabetic() || c == '_')
            .unwrap_or(false)
        && text.chars().all(|c| c.is_alphanumeric() || c == '_')
}

// ------------------------------------------------- групування директив потоку

/// Перетворити плоский список на дерево: `@for` стає [`Each`], ланцюжок
/// `@if`/`@else-if`/`@else` — одним [`Conditional`].
fn finish(source: &Source, items: Vec<Item>) -> Result<Vec<Node>> {
    let nodes = group(source, items)?;
    Ok(merge_text(nodes))
}

/// Сусідні текстові вузли зі суміжними спанами — це один запис у буфер
/// замість кількох.
fn merge_text(nodes: Vec<Node>) -> Vec<Node> {
    let mut out: Vec<Node> = Vec::with_capacity(nodes.len());
    for node in nodes {
        if let (Node::Text(span), Some(Node::Text(previous))) = (&node, out.last_mut()) {
            if previous.end == span.start {
                *previous = Span::new(previous.start, span.end);
                continue;
            }
        }
        out.push(node);
    }
    out
}

fn group(source: &Source, items: Vec<Item>) -> Result<Vec<Node>> {
    let mut out: Vec<Node> = Vec::with_capacity(items.len());
    let mut iter = items.into_iter().peekable();

    while let Some(item) = iter.next() {
        if let Some(span) = item.flow.else_ {
            return Err(Diagnostic::new("`@else` без `@if`", span)
                .with_hint("`@else` має стояти одразу після елемента з `@if`"));
        }
        if let Some(expr) = &item.flow.else_if {
            return Err(Diagnostic::new("`@else-if` без `@if`", expr.span())
                .with_hint("`@else-if` має стояти одразу після елемента з `@if`"));
        }

        let Some(condition) = item.flow.if_.clone() else {
            out.push(wrap_each(item)?);
            continue;
        };

        // `@for` зовнішній, `@if` перевіряється на кожній ітерації (SYNTAX 4.2),
        // тому умова опиняється всередині циклу, а не навколо нього.
        if item.flow.for_.is_some() {
            let Item { node, flow, span } = item;
            let header = flow.for_.expect("перевірено вище");
            let body = Node::Conditional(Box::new(Conditional {
                branches: vec![Branch {
                    condition: Some(condition),
                    body: node,
                }],
                span,
            }));
            let uses_loop = mentions_loop(&body);
            let uses_item = has_expressions(&body);
            out.push(Node::Each(Box::new(Each {
                item: header.item,
                index: header.index,
                list: header.list,
                key: flow.key,
                uses_loop,
                uses_item,
                body,
                span,
            })));
            continue;
        }

        let chain_span = item.span;
        let mut branches = vec![Branch {
            condition: Some(condition),
            body: wrap_each(item)?,
        }];

        // сусідні `@else-if`/`@else`, пропускаючи порожній текст між ними
        let mut skipped: Vec<Node> = Vec::new();
        while let Some(next) = iter.peek() {
            if is_blank(source, next) {
                let blank = iter.next().expect("peek щойно підтвердив елемент");
                skipped.push(blank.node);
                continue;
            }
            let continues = next.flow.else_if.is_some() || next.flow.else_.is_some();
            if !continues {
                break;
            }
            let next = iter.next().expect("peek щойно підтвердив елемент");
            let closes = next.flow.else_.is_some();
            let condition = next.flow.else_if.clone();
            if next.flow.for_.is_some() {
                return Err(
                    Diagnostic::new("`@for` разом із `@else` не підтримується", next.span)
                        .with_hint("винесіть цикл у `<template @for={...}>`"),
                );
            }
            skipped.clear(); // пробіли між гілками нікуди не виводяться
            branches.push(Branch {
                condition,
                body: wrap_each(next)?,
            });
            if closes {
                break;
            }
        }

        out.push(Node::Conditional(Box::new(Conditional {
            branches,
            span: chain_span,
        })));
        out.extend(skipped);
    }

    Ok(out)
}

/// Чи є вузол лише пробілами між гілками `@if`/`@else`.
///
/// Перевіряти треба саме вміст: якщо між гілками стоїть справжній текст, він
/// має лишитись у виводі, а не зникнути разом із розривом ланцюжка.
fn is_blank(source: &Source, item: &Item) -> bool {
    match item.node {
        Node::Text(span) => source.slice(span).trim().is_empty(),
        _ => false,
    }
}

fn wrap_each(item: Item) -> Result<Node> {
    let Item { node, flow, span } = item;
    let Some(header) = flow.for_ else {
        return Ok(node);
    };
    let uses_loop = mentions_loop(&node);
    let uses_item = has_expressions(&node);
    Ok(Node::Each(Box::new(Each {
        item: header.item,
        index: header.index,
        list: header.list,
        key: flow.key,
        uses_loop,
        uses_item,
        body: node,
        span,
    })))
}

/// Чи є в піддереві хоч один вираз.
fn has_expressions(node: &Node) -> bool {
    fn attrs_have(attrs: &[Attribute]) -> bool {
        attrs.iter().any(|attr| match &attr.value {
            AttrValue::Expr(_) | AttrValue::Spread(_) => true,
            AttrValue::Parts(parts) => parts.iter().any(|p| matches!(p, AttrPart::Interp(_))),
            AttrValue::Boolean => false,
        })
    }

    match node {
        Node::Text(_) | Node::Special(_) => false,
        // слот у тілі компонента робить піддерево динамічним
        Node::Slot(_) => true,
        Node::Interp(_) => true,
        Node::Element(element) => {
            !element.bind.is_empty()
                || attrs_have(&element.attrs)
                || element.children.iter().any(has_expressions)
        }
        Node::Component(component) => {
            attrs_have(&component.attrs) || component.children.iter().any(has_expressions)
        }
        Node::Conditional(_) | Node::Each(_) => true,
    }
}

/// Чи згадується `iter` у піддереві. Якщо ні — мапу лічильників не будуємо.
fn mentions_loop(node: &Node) -> bool {
    fn expr_mentions(expr: &Expr) -> bool {
        expr.source().contains("iter")
    }
    fn attrs_mention(attrs: &[Attribute]) -> bool {
        attrs.iter().any(|attr| match &attr.value {
            AttrValue::Expr(expr) | AttrValue::Spread(expr) => expr_mentions(expr),
            AttrValue::Parts(parts) => parts.iter().any(|part| match part {
                AttrPart::Interp(expr) => expr_mentions(expr),
                AttrPart::Text(_) => false,
            }),
            AttrValue::Boolean => false,
        })
    }

    match node {
        Node::Text(_) | Node::Special(_) | Node::Slot(_) => false,
        Node::Interp(interp) => expr_mentions(&interp.expr),
        Node::Element(element) => {
            let bind = &element.bind;
            [
                &bind.class,
                &bind.style,
                &bind.attr,
                &bind.html,
                &bind.text,
                &bind.oob,
            ]
            .into_iter()
            .flatten()
            .any(expr_mentions)
                || attrs_mention(&element.attrs)
                || element.children.iter().any(mentions_loop)
        }
        Node::Component(component) => {
            attrs_mention(&component.attrs) || component.children.iter().any(mentions_loop)
        }
        Node::Conditional(conditional) => conditional.branches.iter().any(|branch| {
            branch
                .condition
                .as_ref()
                .map(expr_mentions)
                .unwrap_or(false)
                || mentions_loop(&branch.body)
        }),
        Node::Each(each) => mentions_loop(&each.body),
    }
}
