//! Рендер дерева в HTML.
//!
//! Дерево незмінне, тож на запит створюється лише буфер і `Scope`. Значення
//! пишуться прямо в буфер (`write_display`), а екранування вибирається за
//! контекстом, порахованим ще при компіляції.

use rhai::{Array, Dynamic, Engine, ImmutableString, Map, Scope};
use rhaix_parser::{Source, Span};
use rhaix_script::{truthy, write_display, Html};

use crate::ast::*;
use crate::error::{Diagnostic, Result};
use crate::escape::{escape_html, is_event_attribute, sanitize_url, Context};
use crate::expr::{Expr, Fast};

/// Те, що ядро підставляє у службові теги layout.
#[derive(Debug, Default, Clone, Copy)]
pub struct Slots<'a> {
    pub slot: &'a str,
    pub head: &'a str,
    pub scripts: &'a str,
}

/// Результат рендеру: HTML і попередження, які не є помилками.
#[derive(Debug, Default)]
pub struct Rendered {
    pub html: String,
    pub warnings: Vec<String>,
}

pub fn render(
    source: &Source,
    engine: &Engine,
    scope: &mut Scope,
    nodes: &[Node],
    slots: Slots<'_>,
) -> Result<Rendered> {
    let mut renderer = Renderer {
        source,
        engine,
        slots,
        out: String::with_capacity(source.text().len() * 2),
        scratch: String::new(),
        warnings: Vec::new(),
        trim_next: false,
    };
    renderer.nodes(nodes, scope)?;
    Ok(Rendered {
        html: renderer.out,
        warnings: renderer.warnings,
    })
}

struct Renderer<'a> {
    source: &'a Source,
    engine: &'a Engine,
    slots: Slots<'a>,
    out: String,
    scratch: String,
    warnings: Vec<String>,
    /// Встановлюється маркером `-}}`: наступний текст іде без початкових пробілів.
    trim_next: bool,
}

impl Renderer<'_> {
    fn nodes(&mut self, nodes: &[Node], scope: &mut Scope) -> Result<()> {
        for node in nodes {
            self.node(node, scope)?;
        }
        Ok(())
    }

    fn node(&mut self, node: &Node, scope: &mut Scope) -> Result<()> {
        match node {
            Node::Text(span) => {
                let text = self.source.slice(*span);
                let text = if self.trim_next {
                    self.trim_next = false;
                    text.trim_start()
                } else {
                    text
                };
                self.out.push_str(text);
                Ok(())
            }
            Node::Interp(interp) => self.interp(interp, scope),
            Node::Element(element) => self.element(element, scope),
            Node::Conditional(conditional) => self.conditional(conditional, scope),
            Node::Each(each) => self.each(each, scope),
            Node::Special(special) => {
                let text = match special {
                    Special::Slot(_) => self.slots.slot,
                    Special::Head(_) => self.slots.head,
                    Special::Scripts(_) => self.slots.scripts,
                };
                self.out.push_str(text);
                Ok(())
            }
            Node::Component(component) => Err(Diagnostic::new(
                format!("компонент `<{}>` ще не підтримується", component.name),
                component.span,
            )
            .with_hint("компоненти, props і слоти з'являться в M3")),
        }
    }

    // ------------------------------------------------------------ вивід

    fn interp(&mut self, interp: &Interp, scope: &mut Scope) -> Result<()> {
        if interp.trim_left {
            let trimmed = self.out.trim_end().len();
            self.out.truncate(trimmed);
        }

        // Швидкий шлях: голе `{{ title }}` або `{{ row.title }}` читається зі
        // scope без виклику рушія (вимір M0: ~73 нс проти ~1 мкс).
        //
        // Значення не клонується: воно позичене зі `scope`, а буфер — у `self`,
        // тож позичення не перетинаються.
        let mut handled = false;
        match interp.expr.fast() {
            Fast::Var(name) => {
                if let Some(value) = scope.get(name) {
                    let value: &Dynamic = value;
                    let (context, span) = (interp.context, interp.expr.span());
                    // SAFETY немає: це звичайні непересічні позичення
                    let out = &mut *self;
                    out.write_value(value, context, span)?;
                    handled = true;
                }
            }
            Fast::Field(var, field) => {
                if let Some(value) = scope.get(var) {
                    if let Some(map) = value.read_lock::<Map>() {
                        if let Some(found) = map.get(field.as_str()) {
                            let (context, span) = (interp.context, interp.expr.span());
                            let out = &mut *self;
                            out.write_value(found, context, span)?;
                            handled = true;
                        }
                    }
                }
            }
            Fast::None => {}
        }

        if !handled {
            let value = interp.expr.eval(self.engine, scope)?;
            self.write_value(&value, interp.context, interp.expr.span())?;
        }

        if interp.trim_right {
            self.trim_next = true;
        }
        Ok(())
    }

    fn write_value(&mut self, value: &Dynamic, context: Context, span: Span) -> Result<()> {
        if let Some(html) = value.read_lock::<Html>() {
            self.out.push_str(html.as_str());
            return Ok(());
        }
        if context == Context::Script {
            return Err(Diagnostic::new(
                "у `<script>` можна виводити лише результат `json(...)`",
                span,
            ));
        }

        // Рядок — найчастіший тип у таблицях: екрануємо прямо з нього,
        // без проміжного буфера.
        if let Some(text) = value.read_lock::<ImmutableString>() {
            match context {
                Context::Url => escape_html(sanitize_url(&text), &mut self.out),
                _ => escape_html(&text, &mut self.out),
            }
            return Ok(());
        }

        let mut scratch = std::mem::take(&mut self.scratch);
        scratch.clear();
        write_display(&mut scratch, value);
        match context {
            Context::Url => escape_html(sanitize_url(&scratch), &mut self.out),
            _ => escape_html(&scratch, &mut self.out),
        }
        self.scratch = scratch;
        Ok(())
    }

    // ---------------------------------------------------------- елементи

    fn element(&mut self, element: &Element, scope: &mut Scope) -> Result<()> {
        let dynamic_class = match &element.bind.class {
            Some(expr) => Some(class_list(&expr.eval(self.engine, scope)?)),
            None => None,
        };
        let dynamic_style = match &element.bind.style {
            Some(expr) => Some(style_list(&expr.eval(self.engine, scope)?)),
            None => None,
        };

        self.out.push('<');
        self.out.push_str(&element.name);

        let mut wrote_class = false;
        let mut wrote_style = false;
        let mut has_id = false;

        for attr in &element.attrs {
            if attr.name == "id" {
                has_id = true;
            }
            match attr.name.as_str() {
                "class" if dynamic_class.is_some() => {
                    wrote_class = true;
                    self.out.push_str(" class=\"");
                    self.attr_value(attr, scope)?;
                    let extra = dynamic_class.as_deref().unwrap_or("");
                    if !extra.is_empty() {
                        self.out.push(' ');
                        escape_html(extra, &mut self.out);
                    }
                    self.out.push('"');
                }
                "style" if dynamic_style.is_some() => {
                    wrote_style = true;
                    self.out.push_str(" style=\"");
                    self.attr_value(attr, scope)?;
                    let extra = dynamic_style.as_deref().unwrap_or("");
                    if !extra.is_empty() {
                        if !self.out.ends_with(';') && !self.out.ends_with('"') {
                            self.out.push(';');
                        }
                        escape_html(extra, &mut self.out);
                    }
                    self.out.push('"');
                }
                _ => self.attribute(attr, scope)?,
            }
        }

        if let (false, Some(classes)) = (wrote_class, dynamic_class.as_deref()) {
            if !classes.is_empty() {
                self.out.push_str(" class=\"");
                escape_html(classes, &mut self.out);
                self.out.push('"');
            }
        }
        if let (false, Some(styles)) = (wrote_style, dynamic_style.as_deref()) {
            if !styles.is_empty() {
                self.out.push_str(" style=\"");
                escape_html(styles, &mut self.out);
                self.out.push('"');
            }
        }

        if let Some(expr) = &element.bind.attr {
            let value = expr.eval(self.engine, scope)?;
            self.spread_attributes(&value, expr.span())?;
        }

        if let Some(expr) = &element.bind.oob {
            let value = expr.eval(self.engine, scope)?;
            self.oob_attributes(&value, has_id);
        }

        self.out.push('>');

        if element.empty {
            if !is_void(&element.name) {
                self.out.push_str("</");
                self.out.push_str(&element.name);
                self.out.push('>');
            }
            return Ok(());
        }

        if let Some(expr) = &element.bind.html {
            let value = expr.eval(self.engine, scope)?;
            self.scratch.clear();
            let mut scratch = std::mem::take(&mut self.scratch);
            write_display(&mut scratch, &value);
            self.out.push_str(&scratch);
            self.scratch = scratch;
        } else if let Some(expr) = &element.bind.text {
            let value = expr.eval(self.engine, scope)?;
            self.write_value(&value, Context::Text, expr.span())?;
        } else {
            self.nodes(&element.children, scope)?;
        }

        self.out.push_str("</");
        self.out.push_str(&element.name);
        self.out.push('>');
        Ok(())
    }

    fn attribute(&mut self, attr: &Attribute, scope: &mut Scope) -> Result<()> {
        match &attr.value {
            AttrValue::Boolean => {
                self.out.push(' ');
                self.out.push_str(&attr.name);
                Ok(())
            }
            AttrValue::Parts(_) => {
                self.out.push(' ');
                self.out.push_str(&attr.name);
                self.out.push_str("=\"");
                self.attr_value(attr, scope)?;
                self.out.push('"');
                Ok(())
            }
            AttrValue::Expr(expr) => {
                let value = expr.eval(self.engine, scope)?;
                // `false` і `()` прибирають атрибут, `true` лишає його без значення
                if value.is_unit() {
                    return Ok(());
                }
                if let Ok(flag) = value.as_bool() {
                    if flag {
                        self.out.push(' ');
                        self.out.push_str(&attr.name);
                    }
                    return Ok(());
                }
                self.out.push(' ');
                self.out.push_str(&attr.name);
                self.out.push_str("=\"");
                let context = attribute_context(&attr.name);
                self.write_value(&value, context, expr.span())?;
                self.out.push('"');
                Ok(())
            }
            AttrValue::Spread(expr) => {
                let value = expr.eval(self.engine, scope)?;
                self.spread_attributes(&value, expr.span())
            }
        }
    }

    /// Вміст значення атрибута (частини тексту й інтерполяції) без лапок.
    fn attr_value(&mut self, attr: &Attribute, scope: &mut Scope) -> Result<()> {
        let AttrValue::Parts(parts) = &attr.value else {
            return Ok(());
        };
        let context = attribute_context(&attr.name);
        for part in parts {
            match part {
                AttrPart::Text(span) => {
                    let text = self.source.slice(*span);
                    if context == Context::Url {
                        escape_html(sanitize_url(text), &mut self.out);
                    } else {
                        escape_html(text, &mut self.out);
                    }
                }
                AttrPart::Interp(expr) => {
                    let value = expr.eval(self.engine, scope)?;
                    self.write_value(&value, context, expr.span())?;
                }
            }
        }
        Ok(())
    }

    fn spread_attributes(&mut self, value: &Dynamic, span: Span) -> Result<()> {
        let Some(map) = value.read_lock::<Map>() else {
            return Err(Diagnostic::new("тут очікується мапа атрибутів", span)
                .with_hint("наприклад, `@attr={#{\"disabled\": locked}}`"));
        };

        let mut pending: Vec<(String, Dynamic)> = Vec::new();
        for (key, value) in map.iter() {
            if is_event_attribute(key) {
                // Обробник події, що приїхав з даних — найпростіший шлях до XSS.
                self.warnings.push(format!(
                    "атрибут `{key}` відкинуто: обробники подій не можна брати з даних"
                ));
                continue;
            }
            pending.push((key.to_string(), value.clone()));
        }
        drop(map);

        for (key, value) in pending {
            if value.is_unit() {
                continue;
            }
            if let Ok(flag) = value.as_bool() {
                if flag {
                    self.out.push(' ');
                    self.out.push_str(&key);
                }
                continue;
            }
            self.out.push(' ');
            self.out.push_str(&key);
            self.out.push_str("=\"");
            let context = attribute_context(&key);
            self.write_value(&value, context, span)?;
            self.out.push('"');
        }
        Ok(())
    }

    /// `@oob={"#row-42"}` → `hx-swap-oob="outerHTML:#row-42"` (плюс `id`, якщо його немає).
    fn oob_attributes(&mut self, value: &Dynamic, has_id: bool) {
        let (selector, swap) = match value.read_lock::<Array>() {
            Some(array) => {
                let selector = array.first().map(rhaix_script::display).unwrap_or_default();
                let swap = array
                    .get(1)
                    .map(rhaix_script::display)
                    .unwrap_or_else(|| "outerHTML".to_owned());
                (selector, swap)
            }
            None => (rhaix_script::display(value), "outerHTML".to_owned()),
        };
        if selector.is_empty() {
            return;
        }

        if !has_id {
            if let Some(id) = selector.strip_prefix('#') {
                self.out.push_str(" id=\"");
                escape_html(id, &mut self.out);
                self.out.push('"');
            }
        }
        self.out.push_str(" hx-swap-oob=\"");
        escape_html(&format!("{swap}:{selector}"), &mut self.out);
        self.out.push('"');
    }

    // ------------------------------------------------------ потік керування

    fn conditional(&mut self, conditional: &Conditional, scope: &mut Scope) -> Result<()> {
        for branch in &conditional.branches {
            match &branch.condition {
                Some(expr) => {
                    let value = eval_condition(expr, self.engine, scope)?;
                    if truthy(&value) {
                        return self.node(&branch.body, scope);
                    }
                }
                None => return self.node(&branch.body, scope),
            }
        }
        Ok(())
    }

    fn each(&mut self, each: &Each, scope: &mut Scope) -> Result<()> {
        let list = each.list.eval(self.engine, scope)?;

        // Колекція не матеріалізується: клонується лише той елемент, який
        // справді потрапляє у scope. Саме тут найлегше випадково скопіювати
        // всю таблицю (RISKS 2.2).
        if let Some(array) = list.read_lock::<Array>() {
            let total = array.len();
            for (index, item) in array.iter().enumerate() {
                self.iteration(each, scope, index, total, item, || {
                    Dynamic::from(index as i64)
                })?;
            }
            return Ok(());
        }
        if let Some(map) = list.read_lock::<Map>() {
            let total = map.len();
            for (index, (key, item)) in map.iter().enumerate() {
                self.iteration(each, scope, index, total, item, || {
                    Dynamic::from(key.to_string())
                })?;
            }
            return Ok(());
        }
        if let Some(range) = list.read_lock::<std::ops::Range<i64>>() {
            let values: Vec<i64> = range.clone().collect();
            let total = values.len();
            for (index, n) in values.into_iter().enumerate() {
                let item = Dynamic::from(n);
                self.iteration(each, scope, index, total, &item, || {
                    Dynamic::from(index as i64)
                })?;
            }
            return Ok(());
        }
        if let Some(range) = list.read_lock::<std::ops::RangeInclusive<i64>>() {
            let values: Vec<i64> = range.clone().collect();
            let total = values.len();
            for (index, n) in values.into_iter().enumerate() {
                let item = Dynamic::from(n);
                self.iteration(each, scope, index, total, &item, || {
                    Dynamic::from(index as i64)
                })?;
            }
            return Ok(());
        }
        if list.is_unit() {
            return Ok(());
        }

        Err(Diagnostic::new(
            "по цьому значенню не можна пройтись циклом",
            each.list.span(),
        )
        .with_hint("`@for` працює з масивом, мапою або діапазоном; тут інший тип"))
    }

    fn iteration(
        &mut self,
        each: &Each,
        scope: &mut Scope,
        index: usize,
        total: usize,
        item: &Dynamic,
        key: impl FnOnce() -> Dynamic,
    ) -> Result<()> {
        let base = scope.len();

        // Якщо в тілі циклу немає жодного виразу, елемент нікуди не потрібен:
        // не клонуємо його і не чіпаємо scope взагалі.
        if each.uses_item || each.index.is_some() || each.key.is_some() {
            scope.push_dynamic(each.item.as_str(), item.clone());
            if let Some(name) = &each.index {
                scope.push_dynamic(name.as_str(), key());
            }
            if each.uses_loop {
                let mut info = Map::new();
                info.insert("index".into(), Dynamic::from(index as i64));
                info.insert("number".into(), Dynamic::from(index as i64 + 1));
                info.insert("first".into(), Dynamic::from(index == 0));
                info.insert("last".into(), Dynamic::from(index + 1 == total));
                info.insert("count".into(), Dynamic::from(total as i64));
                // `loop` — ключове слово Rhai, тому лічильники живуть в `iter`
                scope.push_dynamic("iter", Dynamic::from_map(info));
            }

            // `@key` поки не впливає на вивід — він для morph-свопів (SYNTAX 4.2),
            // але обчислюємо його, щоб помилка у виразі не чекала до v1.1.
            if let Some(key_expr) = &each.key {
                let _ = key_expr.eval(self.engine, scope)?;
            }
        }

        self.node(&each.body, scope)?;
        scope.rewind(base);
        Ok(())
    }
}

/// Умова `@if` обчислюється тим самим шляхом, що й інтерполяція, але без виводу.
fn eval_condition(expr: &Expr, engine: &Engine, scope: &mut Scope) -> Result<Dynamic> {
    if let Fast::Var(name) = expr.fast() {
        if let Some(value) = scope.get(name) {
            return Ok(value.clone());
        }
    }
    expr.eval(engine, scope)
}

/// Контекст значення атрибута визначається його іменем.
fn attribute_context(name: &str) -> Context {
    if crate::escape::is_url_attribute(name) {
        Context::Url
    } else {
        Context::Attribute
    }
}

/// `@class`: мапа вмикає класи за умовою, масив і рядок додають їх як є.
fn class_list(value: &Dynamic) -> String {
    let mut out = String::new();
    if let Some(map) = value.read_lock::<Map>() {
        for (key, flag) in map.iter() {
            if truthy(flag) {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(key);
            }
        }
        return out;
    }
    if let Some(array) = value.read_lock::<Array>() {
        for item in array.iter() {
            let text = rhaix_script::display(item);
            if !text.is_empty() {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(&text);
            }
        }
        return out;
    }
    rhaix_script::display(value)
}

/// `@style`: мапа властивостей; `()` і `false` пропускаються.
fn style_list(value: &Dynamic) -> String {
    let mut out = String::new();
    if let Some(map) = value.read_lock::<Map>() {
        for (key, item) in map.iter() {
            if item.is_unit() || matches!(item.as_bool(), Ok(false)) {
                continue;
            }
            out.push_str(key);
            out.push(':');
            out.push_str(&rhaix_script::display(item));
            out.push(';');
        }
        return out;
    }
    rhaix_script::display(value)
}
