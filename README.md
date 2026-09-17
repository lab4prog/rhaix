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

## Стан: M3 (компоненти)

Що вже працює:

- `rhaix dev <тека>` — маршрути будуються зі структури `pages/`;
- **frontmatter виконується**: `req`, `res`, `hx`, `log`, `state` доступні в кожному `.rhx`;
- **компоненти**: `<TodoItem todo={t} />`, `<Ui.Card>` з props, `{...spread}`, слотами
  (звичайними та іменованими) та ізольованим scope;
- layout вантажиться лише при звичайному заході, на `HX-Request` іде фрагмент;
- `{{ вираз }}` з контекстним екрануванням і директиви `@if` / `@else` / `@for` /
  `@class` / `@style` / `@attr` / `@html` / `@text` / `@oob`;
- форми, валідація зі статусом 422, тости через `HX-Trigger`, редіректи;
- помилки показуються в координатах `.rhx` (`pages/todo.rhx:7:26`) з кареткою й підказкою;
- скрипт обмежений за часом і кількістю операцій — нескінченний цикл дає помилку, не зависання.

У демо працює живий Todo: додати, перемкнути, видалити — уся логіка у
`examples/demo/pages/todo.rhx`.

Невідомий компонент і циклічна залежність — помилка **компіляції**, з підказкою
про схоже ім'я й повним ланцюжком.

Чого ще немає: БД (M5), watcher-а й кешу (M6).

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
| [M2-FINDINGS.md](M2-FINDINGS.md) | frontmatter: пастка `trim()`, кирилиця в заголовках, ворота M2 |
| [M3-FINDINGS.md](M3-FINDINGS.md) | компоненти: діагностика з чужого файлу, слоти проти згортання |
| [examples/demo](examples/demo) | демо, що працює на поточному коді |
| [examples/ergonomics](examples/ergonomics) | найскладніші сторінки, написані руками під спеку |

## Крейти

| Крейт | Роль |
|---|---|
| `rhaix-parser` | джерела, спани, `файл:рядок:колонка`, розділення frontmatter |
| `rhaix-script` | рушій Rhai, ліміти, `display`/`truthy`, `raw()`/`json()`/`url()`, `req`/`res`/`hx`/`state`, бенчмарк |
| `rhaix-template` | лексер `.rhx`, AST, компіляція виразів, компоненти, рендер, екранування |
| `rhaix-server` | axum: маршрути, layout, правило фрагмента, статика |
| `rhaix-cli` | `rhaix dev` |

`rhaix-runtime` поки лишається частиною `rhaix-template`: реєстр компонентів
виявився надто зв'язаним із парсером, щоб ділити їх зараз.

Ліцензія: MIT або Apache-2.0.
