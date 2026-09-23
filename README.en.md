# rhaix

Server-rendered HTML over HTMX, with a component model.
The core is Rust, the scripting language for business logic is
[Rhai](https://rhai.rs), and the developer experience is modelled on Astro.

Ukrainian version: [README.md](README.md).

You write only `.rhx` files: markup plus a small block of logic on top.
No Rust, no frontend build, no `node_modules`.

```
pages/todo.rhx
---
let todos = db.find("todos", #{ done: false }, #{ sort: "id desc" });
page.title = "ToDo";
---
<h1>ToDo</h1>
<ul id="list">
  <TodoItem @for={t in todos} todo={t} />
  <li @if={todos.is_empty()} class="empty">Nothing here</li>
</ul>
```

## Install

A prebuilt `rhaix` (and the `rhaix-lsp` language server) for Linux, macOS and
Windows is attached to every [GitHub release](https://github.com/lab4prog/rhaix/releases).
Or build it from source:

```bash
cargo install rhaix-cli      # the `rhaix` command
cargo install rhaix-lsp      # optional: language server for the VS Code extension
```

```bash
rhaix new myapp && rhaix dev myapp
```

Running an app needs nothing but the `rhaix` binary. `rhaix build` (one
self-contained executable per app) additionally needs a Rust toolchain, 1.88 or
newer; the step-by-step guide is [GUIDE.md](GUIDE.md) (Ukrainian).

## Status: v1.2 — stable

The `.rhx` language, routing, data layer, sessions and batteries are frozen
until 2.0. See [CHANGELOG.md](CHANGELOG.md).

What already works:

- `rhaix dev <dir>` — routes are derived from the structure of `pages/`;
- **frontmatter runs**: `req`, `res`, `hx`, `log`, `state`, `db`, `http`,
  `session`, `csrf` are available in every `.rhx`;
- **components**: `<TodoItem todo={t} />`, `<Ui.Card>` with props, `{...spread}`,
  named and default slots, and an isolated scope;
- **file-based routing**: `pages/` for pages, `partials/` for fragment
  endpoints (`partials/Stats.rhx` → `/components/stats`), `[id].rhx` for dynamic
  segments;
- **`api/` serves JSON**: `api/orders.rhx` → `/api/orders`; a returned map is
  serialised automatically, no layout is applied, and errors and 404s are machine
  readable too. There is no session there on purpose — hence no CSRF to check,
  and no way for another site to act as the logged-in user;
- **`middleware.rhx`** — one file guards every protected page;
- **`@oob`** — one response updates both the main target and a block outside it;
- **database**: a `[db]` section in `rhaix.toml`, migrations from
  `migrations/*.sql` applied at startup, native queries (`db.query`), portable
  CRUD (`db.find/get/insert/…`) with an operator dictionary, and `db.tx` for
  all-or-nothing writes; **two drivers, `sqlite` and `postgres`** — the same
  `.rhx` runs on both, switching driver is a config change, not a code change;
- **sessions and CSRF**: `session.set("user", name)` is a signed cookie that
  survives a server restart. CSRF needs neither switching on nor remembering:
  a form gets a hidden field, a button with `hx-delete` gets a header, and a
  request without a token never reaches `middleware.rhx`;
- **`http`**: `http.get(url).json` straight from frontmatter, with no `await`.
  If the other service is down you get `ok: false`, not a 500 on your page;
- **shared functions**: anything declared in `scripts/*.rhai` is visible from
  every file, with nothing to import;
- **live reload**: an edit is visible in about 70 ms, and the template cache
  knows its dependencies — editing a component refreshes the pages that embed it;
- **component assets**: `<style>` and `<script>` live next to the markup, and the
  core hoists them into the document once; inside a fragment the script is
  wrapped in a registry check, so a repeated swap does not run it again;
- **production build**: `rhaix build` embeds every file into a single binary
  (11.3 MB) that reads nothing from disk except the database; `rhaix serve`
  runs the same app from disk in production mode — frozen cache, compression,
  cached static files;
- the layout is loaded only on a normal visit; on `HX-Request` a fragment is
  returned;
- `{{ expression }}` with context-aware escaping, and the directives `@if` /
  `@else` / `@for` / `@class` / `@style` / `@attr` / `@html` / `@text` / `@oob`;
- forms, validation with status 422, toasts over `HX-Trigger`, redirects;
- errors are reported in `.rhx` coordinates (`pages/todo.rhx:7:26`) with a caret
  and a suggestion;
- scripts are bounded in time and operation count — an infinite loop produces an
  error, not a hang.

Unknown components and circular dependencies are **compile** errors, with a
suggestion for the closest name and the full chain.

Not there yet: password hashing and authentication (M12), `markdown()` (M12),
Postgres / MongoDB / SurrealDB drivers (M11, behind the same `DbDriver` trait).

```bash
cargo run --release -p rhaix-cli -- dev examples/demo --port 3000
```

A new project, and checking one without starting it:

```bash
cargo run --release -p rhaix-cli -- new myapp
```

```bash
cargo run --release -p rhaix-cli -- check myapp --json
```

The whole app in one binary — deployment becomes copying a file:

```bash
cargo run --release -p rhaix-cli -- build myapp
```

Performance gates. M0 measures expression evaluation (4.88 ms against a 5 ms
limit), M1 measures a full 1000-row render (7.34 ms against a 10 ms limit):

```bash
cargo run --release -p rhaix-script --bin rhaix-bench
```

```bash
cargo run --release -p rhaix-template --bin rhaix-render-bench
```

## Documentation

| File | About |
|---|---|
| [SYNTAX.en.md](SYNTAX.en.md) | the full `.rhx` language specification (v1) |
| [llms.txt](llms.txt) | one-file reference written for LLMs |
| [examples/cookbook](examples/cookbook) | "task → finished `.rhx`" recipes, verified by a test |
| [examples/demo](examples/demo) | the demo application running on the current code |
| [bench](bench) | a like-for-like comparison with Astro on a shared database (Ukrainian) |
| [SYNTAX.md](SYNTAX.md) | the Ukrainian specification (the original) |

## Crates

| Crate | Role |
|---|---|
| `rhaix-db` | driver trait, portable CRUD → SQL, SQLite and PostgreSQL drivers, transactions, migrations |
| `rhaix-parser` | sources, spans, `file:line:column`, frontmatter splitting |
| `rhaix-script` | the Rhai engine, limits, `display`/`truthy`, `raw()`/`json()`/`url()`, `req`/`res`/`hx`/`state`, sessions and CSRF, `http`, dates and strings |
| `rhaix-template` | `.rhx` lexer, AST, expression compilation, components, renderer, escaping |
| `rhaix-server` | axum: routing, layout, the fragment rule, static files |
| `rhaix-cli` | `rhaix dev`, `rhaix serve`, `rhaix build`, `rhaix new`, `rhaix check`, `rhaix eject ui` |
| `rhaix-lsp` | language server: diagnostics, go-to-component, completion |

Licence: MIT or Apache-2.0.
