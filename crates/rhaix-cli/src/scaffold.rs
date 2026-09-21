//! `rhaix new` — скелет проєкту.
//!
//! Скелет навмисно маленький: сторінка, layout, компонент, міграція й конфіг.
//! Усе, що з нього видно, — це те, як фреймворк узагалі влаштований, без
//! жодного «розберіться потім».

use std::path::Path;

const CONFIG: &str = r##"[server]
port = 3000

[db]
driver = "sqlite"
url    = "data/app.db"
# Для PostgreSQL — той самий застосунок, інша секція:
# driver = "postgres"
# url    = "postgres://user:pass@localhost:5432/app"
"##;

const LAYOUT: &str = r##"<!DOCTYPE html>
<html lang="uk">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{{ page.title ?? "rhaix" }}</title>
  <rhaix:head />
</head>
<body hx-boost="true">
  {{! `id="nav"` — ціль, у яку кожна сторінка дошле оновлену підсвітку через
      @oob (components/Nav.rhx): layout на htmx-навігації більше не
      рендериться, отже й сам не оновить активний пункт (SYNTAX 6.3, 4.7). }}
  <div id="nav"><Nav /></div>

  <main id="main"><slot /></main>
  <div id="toasts" class="toasts"></div>

  <rhaix:scripts />
</body>
</html>
"##;

const NAV: &str = r##"---
let items = [
    #{ href: "/", label: "Головна" },
    #{ href: "/todo", label: "Справи" },
];
---
<nav>
  <a @for={item in items} href={item.href}
     hx-get={item.href} hx-target="#main" hx-push-url="true"
     @class={#{"active": item.href == req.path}}>{{ item.label }}</a>
</nav>
"##;

const INDEX: &str = r##"---
page.title = "Головна";
let count = db.count("todos");
---
<div @if={req.is_htmx} @oob={"#nav"}><Nav /></div>

<h1>Вітаю!</h1>
<p>
  Це rhaix. Логіка живе у frontmatter цього файлу, розмітка — нижче,
  а сторінки лежать у <code>pages/</code>.
</p>
<p>Справ у базі: <b>{{ count }}</b>.</p>
"##;

const TODO: &str = r##"---
page.title = "Справи";

if req.method == "POST" {
    let title = req.form("title").trim();
    if title != "" {
        db.insert("todos", #{ title: title, done: false });
        hx.toast(`Додано: ${title}`, "success");
    }
}

if req.method == "PATCH" {
    let todo = db.get("todos", req.query_int("id"));
    if todo != () { db.update("todos", todo.id, #{ done: !todo.done }); }
}

if req.method == "DELETE" {
    db.delete("todos", req.query_int("id"));
}

let todos = db.find("todos", #{}, #{ sort: "done asc, id asc" });
---
<div @if={req.is_htmx} @oob={"#nav"}><Nav /></div>

<div id="app">
  <h1>Справи</h1>

  <form hx-post="/todo" hx-target="#app" hx-swap="outerHTML"
        hx-on::after-request="this.reset()">
    <input name="title" placeholder="Що зробити?" autocomplete="off">
    <button>Додати</button>
  </form>

  <ul>
    <TodoItem @for={t in todos} todo={t} />
    <li @if={todos.is_empty()}>Поки порожньо</li>
  </ul>
</div>
"##;

const TODO_ITEM: &str = r##"---
let todo = props.todo;
---
<li @class={#{"done": todo.done}}>
  <input type="checkbox" @attr={#{"checked": todo.done}}
         hx-patch={url("/todo", #{ id: todo.id })}
         hx-target="#app" hx-swap="outerHTML">
  <span>{{ todo.title }}</span>
  <button hx-delete={url("/todo", #{ id: todo.id })}
          hx-target="#app" hx-swap="outerHTML">×</button>
</li>
"##;

const MIGRATION: &str = r##"create table todos (
    id      integer primary key autoincrement,
    title   text    not null,
    done    integer not null default 0,
    created text    not null default (datetime('now'))
);
"##;

const STYLE: &str = r##":root { --fg: #1a1a1a; --line: #e5e7eb; --accent: #b45309 }
* { box-sizing: border-box }
body { margin: 0; font: 16px/1.6 system-ui, sans-serif; color: var(--fg) }
nav { display: flex; gap: 1.25rem; padding: 1rem 1.5rem; border-bottom: 1px solid var(--line) }
nav a { color: inherit; text-decoration: none }
nav a.active { border-bottom: 2px solid var(--accent) }
main { max-width: 44rem; margin: 0 auto; padding: 2rem 1.5rem }
ul { list-style: none; padding: 0 }
li { display: flex; gap: .5rem; align-items: center; padding: .35rem 0 }
li.done span { text-decoration: line-through; opacity: .55 }
.toasts { position: fixed; right: 1rem; bottom: 1rem; display: grid; gap: .5rem }
.toast { padding: .6rem 1rem; border-radius: 6px; color: #fff; background: #334155 }
.toast.success { background: #15803d } .toast.error { background: #b91c1c }
"##;

const APP_JS: &str = r##"// Тости з HX-Trigger: сервер шле подію, клієнт показує повідомлення.
document.body.addEventListener("showToast", (event) => {
  const { message, type } = event.detail ?? {};
  const el = document.createElement("div");
  el.className = `toast ${type ?? "info"}`;
  el.textContent = message ?? "";
  document.getElementById("toasts")?.appendChild(el);
  setTimeout(() => el.remove(), 3000);
});
"##;

const GITIGNORE: &str = "\
data/
# Ключ підпису сесій. Генерується при першому `rhaix dev` і не має потрапляти
# в репозиторій: у продакшні його задають через RHAIX_SECRET.
.rhaix-secret
";

/// Створити новий проєкт у теці `path`.
pub fn create(path: &Path) -> anyhow::Result<()> {
    if path.exists() && path.read_dir()?.next().is_some() {
        anyhow::bail!("тека `{}` не порожня", path.display());
    }

    let files: [(&str, &str); 10] = [
        ("rhaix.toml", CONFIG),
        ("layouts/main.rhx", LAYOUT),
        ("pages/index.rhx", INDEX),
        ("pages/todo.rhx", TODO),
        ("components/Nav.rhx", NAV),
        ("components/TodoItem.rhx", TODO_ITEM),
        ("migrations/001_todos.sql", MIGRATION),
        ("public/style.css", STYLE),
        ("public/app.js", APP_JS),
        (".gitignore", GITIGNORE),
    ];

    for (name, body) in files {
        let file = path.join(name);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&file, body)?;
    }

    println!("Проєкт створено: {}", path.display());
    println!();
    println!("  cd {}", path.display());
    println!("  rhaix dev");
    println!();
    println!("Далі: сторінки — у `pages/`, компоненти — у `components/`,");
    println!("схема бази — у `migrations/`, налаштування — у `rhaix.toml`.");
    Ok(())
}
