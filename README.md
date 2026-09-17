# rhaix

Серверний рендер HTML на HTMX з компонентним підходом.
Ядро — Rust, скриптова мова для бізнес-логіки — [Rhai](https://rhai.rs), DX — як в Astro.

Людина пише тільки файли `.rhx`: розмітка плюс невеликий блок логіки згори.
Ніякого Rust, ніякої збірки фронтенду, ніякого `node_modules`.

```
pages/todo.rhx
---
let todos = db.find("todos", #{ done: false }, #{ sort: "id desc" });
page.title = "ToDo";
---
<h1>ToDo</h1>
<ul id="list">
  <TodoItem @for={t in todos} todo={t} />
  <li @if={todos.is_empty()} class="empty">Порожньо</li>
</ul>
```

## Стан: M1 (шаблонізатор)

Що вже працює:

- `rhaix dev <тека>` — маршрути будуються зі структури `pages/`;
- layout вантажиться лише при звичайному заході, на `HX-Request` іде фрагмент;
- `{{ вираз }}` з контекстним екрануванням і директиви `@if` / `@else` / `@for` /
  `@class` / `@style` / `@attr` / `@html` / `@text` / `@oob`;
- `<slot/>`, `<rhaix:head/>` і `<rhaix:scripts/>` — вузли шаблону, а не заміни рядків;
- помилки показуються в координатах `.rhx` (`pages/todo.rhx:7:26`) з кареткою й підказкою.

Чого ще немає: виконання frontmatter (M2), компонентів (M3), БД (M5), watcher-а (M6).
Дані на сторінках демо поки підставляє сервер.

```bash
cargo run --release -p rhaix-cli -- dev examples/demo --port 3000
```

Ворота продуктивності. M0 міряє обчислення виразів (4.88 мс при межі 5 мс),
M1 — повний рендер 1000 рядків (7.34 мс при межі 10 мс):

```bash
cargo run --release -p rhaix-script --bin rhaix-bench
```

```bash
cargo run --release -p rhaix-template --bin rhaix-render-bench
```

## Документи

| Файл | Про що |
|---|---|
| [SYNTAX.md](SYNTAX.md) | повна специфікація мови `.rhx` (v1) |
| [PLAN.md](PLAN.md) | архітектура, API, дорожня карта M0-M13 |
| [RISKS.md](RISKS.md) | підводні камені, ворота рішень, оцінка життєздатності |
| [M0-FINDINGS.md](M0-FINDINGS.md) | результати спайку: виміри й сім знайдених тертя |
| [M1-FINDINGS.md](M1-FINDINGS.md) | шаблонізатор: три зміни в спеці й оптимізації рендеру |
| [examples/demo](examples/demo) | демо, що працює на поточному коді |
| [examples/ergonomics](examples/ergonomics) | найскладніші сторінки, написані руками під спеку |

## Крейти

| Крейт | Роль |
|---|---|
| `rhaix-parser` | джерела, спани, `файл:рядок:колонка`, розділення frontmatter |
| `rhaix-script` | рушій Rhai, ліміти, `display`/`truthy`, `raw()`/`json()`/`url()`, бенчмарк |
| `rhaix-template` | лексер `.rhx`, AST, компіляція виразів, рендер, екранування |
| `rhaix-server` | axum: маршрути, layout, правило фрагмента, статика |
| `rhaix-cli` | `rhaix dev` |

`rhaix-runtime` відокремиться від `rhaix-template` у M3, коли з'явиться реєстр
компонентів.

Ліцензія: MIT або Apache-2.0.
