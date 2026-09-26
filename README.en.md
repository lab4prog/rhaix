# rhaix

Ukrainian version: [README.md](README.md).

Server-rendered HTML over HTMX, with a component model.
The core is Rust, the scripting language for business logic is
[Rhai](https://rhai.rs), and the developer experience is modelled on Astro.

You write only `.rhx` files: markup plus a small block of logic on top.
No Rust, no frontend build, no `node_modules`. Deployment is copying one file.

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

Archives with `rhaix` and the `rhaix-lsp` language server for Linux, macOS and
Windows are attached to every [GitHub release](https://github.com/lab4prog/rhaix/releases).
Or build from source (Rust 1.88+):

```bash
cargo install --git https://github.com/lab4prog/rhaix rhaix-cli
```

```bash
rhaix new myapp && rhaix dev myapp
```

Developing and running an app needs nothing but the `rhaix` binary.
`rhaix build` (one self-contained executable per app) additionally needs a Rust
toolchain. The step-by-step guide is [GUIDE.md](GUIDE.md) (Ukrainian).

## Status

The current version is **1.6.4**. The `.rhx` language, routing, data layer,
sessions and batteries are stable: breaking changes can only come in 2.0.
History is in [CHANGELOG.md](CHANGELOG.md) (Ukrainian).

## What's inside

**Language and components**

- `{{ expression }}` escaped for its context: text, attribute, URL, `<script>`;
  `onclick={…}` from data is a compile error;
- directives `@if` / `@else-if` / `@else` / `@for` / `@class` / `@style` /
  `@attr` / `@html` / `@text` / `@oob` — the last one updates a block outside the
  main target with the same response;
- components `<TodoItem todo={t} />`, `<Ui.Card>` with props, `{...spread}`,
  default and named slots, and an isolated scope;
- `<style>` and `<script>` next to a component's markup; the core hoists them
  into the document once, and `<style scoped>` narrows selectors to its own file;
- shared functions in `scripts/*.rhai` are visible from every file, with
  nothing to import.

**Routing and HTTP**

- file-based routes: `pages/` for pages, `partials/` for fragments, `[id].rhx`
  for dynamic segments;
- the fragment rule: an htmx request gets the page's markup without the layout,
  a boosted navigation gets the full page;
- `api/` serves JSON: a returned map is serialised automatically, errors are
  JSON too, and there is deliberately no session; CORS in one line of config,
  and `rhaix openapi` derives an OpenAPI 3.1 spec from the files themselves;
- `middleware.rhx` guards every page from one file; a ready recipe for roles
  and permissions lives in [examples/cookbook](examples/cookbook);
- live updates: `live.send("orders")` on the server, and every open page
  listening with `hx-trigger="live:orders from:body"` re-fetches its data.

**Data**

- two drivers, `sqlite` and `postgres`: the same `.rhx` runs on both, and
  switching driver is a `rhaix.toml` change;
- portable CRUD (`db.find/get/insert/update/delete`) with an operator dictionary,
  plus your own SQL with `?` placeholders; `db.tx` for all-or-nothing writes;
- migrations from `migrations/*.sql` applied at startup;
- `db.grid` — an admin table in one call, with sorting, filters and pages kept
  in the URL; `db.attach` removes the query-per-row pattern;
- a bounded connection pool and a prepared-statement cache on both drivers.

**Security**

- a session is a signed cookie that survives a restart and slides while it is
  used;
- CSRF needs no switching on: a form gets a hidden field, a button with
  `hx-delete` gets a header, and a request without a token never reaches
  `middleware.rhx`;
- passwords with Argon2id (`hash_password`/`verify_password`), rate limiting
  with `state.allow(key, max, seconds)`;
- scripts are bounded in time and operation count: an infinite loop is an error,
  not a hang.

**Batteries**

- `validate()` checks a form in one call, `paginate()` does all the page
  arithmetic;
- file uploads with path checks, mail (`mail.send`; without SMTP it goes to the
  log);
- `http.get(url).json` straight from frontmatter, no `await`; a service that is
  down gives `ok: false`, not a 500;
- exports: `res.download("report.csv", csv(rows, #{ columns: [...] }))` —
  RFC 4180, formula cells defused, a BOM for Excel, non-ASCII file names;
- dates, `slug()` with transliteration, `money()`, translations with `t("key")`,
  and a safe `markdown()` — raw HTML is escaped, link schemes are checked;
- your own Rust functions in `native/lib.rs` — for when Rhai is not enough.

**Client**

- htmx ships inside the binary: no CDN dependency;
- the `rhaix.js` core swaps `422`/`403`/`404` responses too, so validation
  errors show up on screen;
- `hx.toast(...)` survives a redirect, and `<dialog>` becomes a real modal on its
  own; override the interface with one function or take it over completely
  (`rhaix eject ui`).

**Tooling and deployment**

- `rhaix dev` reloads in about 70 ms; the cache knows its dependencies, so
  editing a component refreshes the pages that embed it;
- errors in `.rhx` coordinates (`pages/todo.rhx:7:26`) with a caret and a
  suggestion; an unknown component or a dependency cycle is a compile error;
- `rhaix check` validates a project without starting it and warns about common
  traps;
- `rhaix build` embeds every file into one binary that reads nothing from disk
  except the database; `rhaix serve` runs the same app from disk in production
  mode;
- a VS Code extension with a language server: diagnostics, `F12` to a component,
  completion ([editors/vscode](editors/vscode)).

## What's not there

- **Islands and client components** — on purpose: interactivity is htmx
  attributes, `public/*.js` and component scripts.
- **MongoDB and SurrealDB drivers** — the `DbDriver` trait is ready for them.
- **Scoped slots, `@key` morph swaps, `@transition`** — the names are reserved.
- **Rename and find-references** in the editor.

## Documentation

| File | About |
|---|---|
| [SYNTAX.en.md](SYNTAX.en.md) | the full reference: `.rhx`, globals, `rhaix.toml` |
| [llms.txt](llms.txt) | one-file reference written for LLMs |
| [examples/cookbook](examples/cookbook) | "task → finished `.rhx`" recipes, verified by a test |
| [examples/demo](examples/demo) | demo: todo, sign-in, admin, escaping examples |
| [examples/ergonomics](examples/ergonomics) | the hardest pages, written by hand against the spec |
| [editors/vscode](editors/vscode) | VS Code extension: highlighting and the language server |
| [GUIDE.md](GUIDE.md) | guide: create an app, database and migrations, deploy, update (Ukrainian) |
| [SYNTAX.md](SYNTAX.md) | the Ukrainian reference (the original) |
| [bench](bench) | a like-for-like comparison with Astro on a shared database (Ukrainian) |
| [CHANGELOG.md](CHANGELOG.md) | changes by version (Ukrainian) |
| [RELEASING.md](RELEASING.md) | how to cut a release (Ukrainian) |

## Working on rhaix itself

The demo from a clone, without installing:

```bash
cargo run --release -p rhaix-cli -- dev examples/demo --port 3000
```

The same checks CI runs:

```bash
cargo test --workspace --locked
```

```bash
cargo clippy --workspace --all-targets --locked
```

```bash
cargo fmt --all --check
```

Core benchmarks with limits that must not be exceeded: expression evaluation
(5 ms) and a full 1000-row table render (10 ms):

```bash
cargo run --release -p rhaix-script --bin rhaix-bench
```

```bash
cargo run --release -p rhaix-template --bin rhaix-render-bench
```

| Crate | Role |
|---|---|
| `rhaix-db` | driver trait, portable CRUD → SQL, SQLite and PostgreSQL drivers, pool, transactions, migrations |
| `rhaix-parser` | sources, spans, `file:line:column`, frontmatter splitting |
| `rhaix-script` | the Rhai engine with limits, `req`/`res`/`hx`/`state`, sessions and CSRF, `http`, `db.grid`, `csv`, dates and strings |
| `rhaix-template` | `.rhx` lexer, AST, expression compilation, components, renderer, escaping, scoped CSS |
| `rhaix-server` | axum: routing, layout, the fragment rule, `api/`, live updates, static files, `rhaix check` |
| `rhaix-cli` | commands `new`, `dev`, `serve`, `build`, `check`, `openapi`, `eject ui` |
| `rhaix-lsp` | language server: diagnostics, go-to-component, completion |

Licence: MIT or Apache-2.0.
