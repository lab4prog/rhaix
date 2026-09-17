//! Дерево шаблону.
//!
//! Дерево незмінне і спільне для всіх запитів (`Arc<Template>`): на запит
//! змінюється лише `Scope`. Тому директиви тут уже розібрані, вирази
//! скомпільовані, а `@if`/`@for` перетворені на вузли [`Conditional`] і [`Each`].

use rhaix_parser::Span;

use crate::escape::Context;
use crate::expr::Expr;

#[derive(Debug, Clone)]
pub enum Node {
    /// Текст віддається дослівно — це більшість будь-якого шаблону.
    Text(Span),
    Interp(Interp),
    Element(Box<Element>),
    /// Компонент розбирається вже зараз, але рендериться з M3.
    Component(Box<Component>),
    Conditional(Box<Conditional>),
    Each(Box<Each>),
    Special(Special),
}

#[derive(Debug, Clone)]
pub struct Interp {
    pub expr: Expr,
    pub context: Context,
    pub trim_left: bool,
    pub trim_right: bool,
}

#[derive(Debug, Clone)]
pub struct Element {
    pub name: String,
    pub name_span: Span,
    pub attrs: Vec<Attribute>,
    pub bind: Bindings,
    pub children: Vec<Node>,
    /// Порожній елемент: `<br>` або `<div/>`.
    pub empty: bool,
    pub span: Span,
}

/// Директиви, що змінюють атрибути або вміст елемента.
#[derive(Debug, Clone, Default)]
pub struct Bindings {
    pub class: Option<Expr>,
    pub style: Option<Expr>,
    pub attr: Option<Expr>,
    pub html: Option<Expr>,
    pub text: Option<Expr>,
    pub oob: Option<Expr>,
}

impl Bindings {
    /// Чи немає жодної директиви, що змінює атрибути або вміст.
    pub fn is_empty(&self) -> bool {
        self.class.is_none()
            && self.style.is_none()
            && self.attr.is_none()
            && self.html.is_none()
            && self.text.is_none()
            && self.oob.is_none()
    }
}

#[derive(Debug, Clone)]
pub struct Attribute {
    /// Для `{...spread}` ім'я порожнє.
    pub name: String,
    pub span: Span,
    pub value: AttrValue,
}

#[derive(Debug, Clone)]
pub enum AttrValue {
    /// `<input required>`
    Boolean,
    /// `href="/todo/{{ id }}"`
    Parts(Vec<AttrPart>),
    /// `href={link}`
    Expr(Expr),
    /// `{...attrs}`
    Spread(Expr),
}

#[derive(Debug, Clone)]
pub enum AttrPart {
    Text(Span),
    Interp(Expr),
}

#[derive(Debug, Clone)]
pub struct Conditional {
    pub branches: Vec<Branch>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Branch {
    /// `None` — це `@else`.
    pub condition: Option<Expr>,
    pub body: Node,
}

#[derive(Debug, Clone)]
pub struct Each {
    pub item: String,
    pub index: Option<String>,
    pub list: Expr,
    pub key: Option<Expr>,
    /// Чи згадується `iter` у тілі. Якщо ні — мапу лічильників не будуємо
    /// (на 1000 рядків це помітно).
    pub uses_loop: bool,
    /// Чи є в тілі хоч один вираз. Якщо ні — елемент циклу нікуди не потрібен,
    /// і його не треба ні клонувати, ні класти в scope.
    pub uses_item: bool,
    pub body: Node,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Component {
    pub name: String,
    pub attrs: Vec<Attribute>,
    pub children: Vec<Node>,
    pub span: Span,
}

/// Службові теги, які розкриває ядро.
#[derive(Debug, Clone, Copy)]
pub enum Special {
    /// `<slot />` — сюди layout вставляє сторінку.
    Slot(Span),
    /// `<rhaix:head />`
    Head(Span),
    /// `<rhaix:scripts />`
    Scripts(Span),
}

/// Елементи, які не мають закривального тега.
pub fn is_void(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

/// Елементи, вміст яких не є розміткою.
pub fn is_raw_text(name: &str) -> bool {
    matches!(name, "script" | "style" | "pre" | "textarea")
}

/// Контекст виводу всередині елемента (SYNTAX 2.5).
pub fn content_context(name: &str) -> Context {
    match name {
        "script" => Context::Script,
        _ => Context::Text,
    }
}
