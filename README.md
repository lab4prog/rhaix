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

## Стан: M0 (скелет і спайк)

Що вже працює:

- `rhaix dev <тека>` — маршрути будуються зі структури `pages/`;
- layout вантажиться лише при звичайному заході, на `HX-Request` іде фрагмент;
- `<rhaix:head/>` і `<rhaix:scripts/>` самі підключають `public/**.css` і `public/**.js`;
- frontmatter відрізається й не потрапляє у вивід;
- помилки показуються в координатах `.rhx` (`pages/todo.rhx:7:26`).

Чого ще немає: рендеру `{{ }}`, директив, компонентів, виконання логіки,
watcher-а, БД. Це M1-M5.

```bash
cargo run --release -p rhaix-cli -- dev examples/demo --port 3000
```

Ворота продуктивності M0 (1000 рядків × 10 виразів — 4.88 мс при межі 5 мс):

```bash
cargo run --release -p rhaix-script --bin rhaix-bench
```

## Документи

| Файл | Про що |
|---|---|
| [SYNTAX.md](SYNTAX.md) | повна специфікація мови `.rhx` (v1) |
| [PLAN.md](PLAN.md) | архітектура, API, дорожня карта M0-M13 |
| [RISKS.md](RISKS.md) | підводні камені, ворота рішень, оцінка життєздатності |
| [M0-FINDINGS.md](M0-FINDINGS.md) | результати спайку: виміри й сім знайдених тертя |
| [examples/demo](examples/demo) | демо, що працює на поточному коді |
| [examples/ergonomics](examples/ergonomics) | найскладніші сторінки, написані руками під спеку |

## Крейти

| Крейт | Роль |
|---|---|
| `rhaix-parser` | джерела, спани, `файл:рядок:колонка`, розділення frontmatter |
| `rhaix-script` | рушій Rhai, ліміти, правила `display`/`truthy`, бенчмарк |
| `rhaix-server` | axum: маршрути, layout, правило фрагмента, статика |
| `rhaix-cli` | `rhaix dev` |

`rhaix-template` і `rhaix-runtime` з'являться в M1.

Ліцензія: MIT або Apache-2.0.
