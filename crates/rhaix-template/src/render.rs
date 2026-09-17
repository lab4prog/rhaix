//! Рендер дерева в HTML.
//!
//! Дерево незмінне, тож на запит створюється лише буфер і `Scope`. Значення
//! пишуться прямо в буфер (`write_display`), а екранування вибирається за
//! контекстом, порахованим ще при компіляції.
//!
//! Компонент рендериться у **власному** `Scope`: він бачить лише свої props,
//! слоти й глобальні об'єкти. Змінні батька йому недоступні — саме це робить
//! компонент переносимим (SYNTAX 5.3).

use std::collections::BTreeMap;

use rhai::{Array, Dynamic, Engine, ImmutableString, Map, Scope};
use rhaix_parser::{Source, Span};
use rhaix_script::{truthy, write_display, Html, SlotSet};

use crate::ast::*;
use crate::error::{Diagnostic, Result};
use crate::escape::{escape_html, is_event_attribute, sanitize_url, Context};
use crate::expr::{Expr, Fast};

/// Скільки рівнів вкладених компонентів дозволено.
///
/// Циклічні залежності ловляться ще при компіляції, тож сюди можна дійти лише
/// дуже глибокою (але скінченною) вкладеністю.
const MAX_DEPTH: usize = 32;

/// Значення, які бачить кожен файл: `req`, `res`, `hx`, `state`, `log`, `page`.
///
/// Компонент не успадковує scope батька, тому глобальні об'єкти передаються
/// явно — інакше в компоненті не було б ні `req`, ні `page`.
#[derive(Debug, Default, Clone)]
pub struct Globals {
    entries: Vec<(String, Dynamic)>,
}

impl Globals {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, name: impl Into<String>, value: Dynamic) -> &mut Self {
        self.entries.push((name.into(), value));
        self
    }

    /// Покласти глобальні об'єкти у свіжий scope.
    pub fn apply(&self, scope: &mut Scope) {
        for (name, value) in &self.entries {
            scope.push_dynamic(name.as_str(), value.clone());
        }
    }
}

/// Те, що ядро підставляє у службові теги.
#[derive(Debug, Default, Clone, Copy)]
pub struct Slots<'a> {
    /// Вміст `<slot/>` верхнього рівня — сторінка для layout.
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

pub fn render<'a>(
    source: &'a Source,
    engine: &'a Engine,
    scope: &mut Scope,
    nodes: &'a [Node],
    slots: Slots<'a>,
    globals: &'a Globals,
) -> Result<Rendered> {
    let mut frame = SlotFrame::new();
    if !slots.slot.is_empty() {
        frame.insert(String::new(), slots.slot.to_owned());
    }

    let mut renderer = Renderer {
        source,
        engine,
        globals,
        slots,
        frames: vec![frame],
        depth: 0,
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

/// Слоти одного рівня: `""` — слот за замовчуванням.
type SlotFrame = BTreeMap<String, String>;

struct Renderer<'a> {
    /// Джерело поточного файлу. Під час рендеру компонента підмінюється на його
    /// власне — інакше спани текстових вузлів вказували б не туди.
    source: &'a Source,
    engine: &'a Engine,
    globals: &'a Globals,
    slots: Slots<'a>,
    frames: Vec<SlotFrame>,
    depth: usize,
    out: String,
    /// Буфери, що живуть довше за один вузол: інакше кожен рядок таблиці
    /// коштував би кількох алокацій.
    scratch: String,
    warnings: Vec<String>,
    /// Встановлюється маркером `-}}`: наступний текст іде без початкових пробілів.
    trim_next: bool,
}

impl<'a> Renderer<'a> {
    fn nodes(&mut self, nodes: &'a [Node], scope: &mut Scope) -> Result<()> {
        for node in nodes {
            self.node(node, scope)?;
        }
        Ok(())
    }

    fn node(&mut self, node: &'a Node, scope: &mut Scope) -> Result<()> {
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
            Node::Component(component) => self.component(component, scope),
            Node::Slot(slot) => self.slot(slot, scope),
            Node::Special(special) => {
                let text = match special {
                    Special::Head(_) => self.slots.head,
                    Special::Scripts(_) => self.slots.scripts,
                };
                self.out.push_str(text);
                Ok(())
            }
        }
    }

    // ------------------------------------------------------------ вивід

    fn interp(&mut self, interp: &'a Interp, scope: &mut Scope) -> Result<()> {
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
                    let (context, span) = (interp.context, interp.expr.span());
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

    fn element(&mut self, element: &'a Element, scope: &mut Scope) -> Result<()> {
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
            let mut scratch = std::mem::take(&mut self.scratch);
            scratch.clear();
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

    fn attribute(&mut self, attr: &'a Attribute, scope: &mut Scope) -> Result<()> {
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
    fn attr_value(&mut self, attr: &'a Attribute, scope: &mut Scope) -> Result<()> {
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

    fn conditional(&mut self, conditional: &'a Conditional, scope: &mut Scope) -> Result<()> {
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

    fn each(&mut self, each: &'a Each, scope: &mut Scope) -> Result<()> {
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
        each: &'a Each,
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

    // ---------------------------------------------------------- компоненти

    fn component(&mut self, component: &'a Component, scope: &mut Scope) -> Result<()> {
        if self.depth >= MAX_DEPTH {
            return Err(Diagnostic::new(
                format!("забагато вкладених компонентів (більше {MAX_DEPTH})"),
                component.span,
            ));
        }

        // 1. props обчислюються у scope батька
        let props = self.collect_props(component, scope)?;

        // 2. слоти теж рендеряться у scope батька (SYNTAX 5.4)
        let mut frame = SlotFrame::new();
        if !component.children.is_empty() {
            let html = self.capture(&component.children, scope)?;
            frame.insert(String::new(), html);
        }
        for (name, nodes) in &component.named {
            let html = self.capture(nodes, scope)?;
            frame.insert(name.clone(), html);
        }

        // 3. свій scope: props, слоти й глобальні об'єкти — і більше нічого
        let mut child = Scope::new();
        self.globals.apply(&mut child);
        for (key, value) in props.iter() {
            if is_plain_name(key) {
                child.push_dynamic(key.as_str(), value.clone());
            }
        }
        child.push_dynamic("props", Dynamic::from_map(props));
        child.push(
            "slots",
            SlotSet::new(frame.keys().filter(|k| !k.is_empty()).cloned().collect()),
        );

        let template = &component.template;
        let frame_name = template.source().path().display().to_string();

        // 4. логіка компонента
        let returned = template
            .run_script(self.engine, &mut child)
            .map_err(|diagnostic| {
                diagnostic
                    .in_file(template.source_arc())
                    .in_frame(frame_name.clone())
            })?;
        if !returned.is_unit() {
            // Компонент, як і сторінка, може віддати готове тіло замість розмітки
            self.out.push_str(&rhaix_script::display(&returned));
            return Ok(());
        }

        // 5. рендер розмітки компонента — у його власному джерелі
        let previous_source = std::mem::replace(&mut self.source, template.source());
        self.frames.push(frame);
        self.depth += 1;

        let result = self
            .nodes(template.nodes(), &mut child)
            .map_err(|diagnostic| {
                diagnostic
                    .in_file(template.source_arc())
                    .in_frame(frame_name)
            });

        self.depth -= 1;
        self.frames.pop();
        self.source = previous_source;
        result
    }

    /// Обчислити props у порядку запису: пізніший `{...spread}` перекриває раніші.
    fn collect_props(&mut self, component: &'a Component, scope: &mut Scope) -> Result<Map> {
        let mut props = Map::new();
        for attr in &component.attrs {
            match &attr.value {
                AttrValue::Boolean => {
                    props.insert(attr.name.as_str().into(), Dynamic::from(true));
                }
                AttrValue::Expr(expr) => {
                    let value = expr.eval(self.engine, scope)?;
                    props.insert(attr.name.as_str().into(), value);
                }
                AttrValue::Parts(parts) => {
                    let mut text = String::new();
                    for part in parts {
                        match part {
                            AttrPart::Text(span) => text.push_str(self.source.slice(*span)),
                            AttrPart::Interp(expr) => {
                                let value = expr.eval(self.engine, scope)?;
                                write_display(&mut text, &value);
                            }
                        }
                    }
                    props.insert(attr.name.as_str().into(), Dynamic::from(text));
                }
                AttrValue::Spread(expr) => {
                    let value = expr.eval(self.engine, scope)?;
                    let Some(map) = value.read_lock::<Map>() else {
                        return Err(Diagnostic::new("розпакувати можна лише мапу", expr.span()));
                    };
                    for (key, value) in map.iter() {
                        props.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        Ok(props)
    }

    /// Відрендерити вузли в окремий рядок — так збирається вміст слота.
    fn capture(&mut self, nodes: &'a [Node], scope: &mut Scope) -> Result<String> {
        let saved = std::mem::take(&mut self.out);
        let saved_trim = std::mem::replace(&mut self.trim_next, false);
        let result = self.nodes(nodes, scope);
        let captured = std::mem::replace(&mut self.out, saved);
        self.trim_next = saved_trim;
        result.map(|_| captured)
    }

    fn slot(&mut self, slot: &'a SlotNode, scope: &mut Scope) -> Result<()> {
        let name = slot.name.clone().unwrap_or_default();
        let content = self
            .frames
            .last()
            .and_then(|frame| frame.get(&name))
            .cloned();

        match content {
            Some(html) if !html.trim().is_empty() => {
                self.out.push_str(&html);
                Ok(())
            }
            // слот не передали — показуємо запасний вміст
            _ => self.nodes(&slot.fallback, scope),
        }
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

/// Чи можна зробити з імені prop-а звичайну змінну (`data-x` — ні).
fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .next()
            .map(|ch| ch.is_alphabetic() || ch == '_')
            .unwrap_or(false)
        && name.chars().all(|ch| ch.is_alphanumeric() || ch == '_')
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
