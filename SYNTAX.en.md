# rhaix — `.rhx` syntax specification (v1)

Anything marked **[v1.1]** is deliberately out of scope for the first version.
Ukrainian original: [SYNTAX.md](SYNTAX.md). For LLMs: [llms.txt](llms.txt).
Ready-made recipes: [examples/cookbook](examples/cookbook).

> Note on language: diagnostics are currently emitted in Ukrainian. A `lang`
> switch is planned; this document translates the messages where it quotes them.

---

## 0. One-screen cheat sheet

```
---                                   // frontmatter: Rhai, never reaches the output
let todos = db.query("select * from todos");
let title = "ToDo";
page.title = title;
---
<h1>{{ title }}</h1>                       {{ expr }}         — escaped output
<div>{{ raw(post.body) }}</div>            raw(...)           — no escaping
{{! this is a comment, invisible in the HTML }}

<ul>
  <TodoItem @for={t in todos} @key={t.id} todo={t} />        loop
  <li @if={todos.is_empty()}>Nothing here</li>               condition
  <li @else>Total: {{ todos.len() }}</li>                    otherwise
</ul>

<a href="/todo/{{ t.id }}"                 interpolation inside an attribute
   class="btn" @class={#{"active": is_on}} @attr={#{"disabled": locked}}>
  {{ t.title }}
</a>

<Card>                                     component with a slot
  <template slot="header"><h2>Heading</h2></template>
  Body
</Card>
```

Four rules, and there is nothing else to memorise:

1. A tag starting with a **capital** letter → a component from `components/`.
2. An attribute starting with `@` → a framework directive, never emitted.
3. `{ ... }` as an attribute value → a Rhai expression; `"..."` → a string
   (which may contain `{{ }}`).
4. Everything else is ordinary HTML, emitted as written.

---

## 1. The `.rhx` file

```
file        = [ frontmatter ] , template ;
frontmatter = "---" , NEWLINE , rhai-code , NEWLINE , "---" , NEWLINE ;
```

- The opening `---` must be the **first** non-empty line of the file.
- Between the `---` markers is [Rhai](https://rhai.rs) code, executed on the
  server before the markup is rendered. **Nothing** from that block reaches the
  response.
- Frontmatter is optional — a file may be plain HTML, which is fine and normal.
- Encoding is UTF-8. Line endings LF or CRLF (normalised).

### 1.1 Kinds of file

| Directory | Role | Reachable over HTTP |
|---|---|---|
| `pages/**/*.rhx` | page / endpoint | yes, by file path |
| `partials/**/*.rhx` | HTMX fragment | yes, `/components/<name>` |
| `components/**/*.rhx` | component | **no** |
| `layouts/*.rhx` | full-page wrapper | no |
| `scripts/*.rhai` | shared Rhai functions, visible everywhere without importing | no |
| `middleware.rhx` | code that runs before every request (6.6) | no |
| `api/**/*.rhx` | JSON endpoint (6.7) | yes, at `/api/<path>` |

---

## 2. Text, output and escaping

### 2.1 Interpolation

```
interp = "{{" , [ "-" ] , rhai-expr , [ "-" ] , "}}" ;
```

```html
<p>{{ user.name }}</p>
<p>{{ price * qty }} UAH</p>
<p>{{ if done { "✔" } else { "…" } }}</p>
<p>{{ todos.filter(|t| !t.done).len() }}</p>
```

- Inside is **any Rhai expression** (not a statement: no `let`, `;`, `while`).
- The expression is compiled once when the file is loaded and cached as a
  `rhai::AST`.
- The result is converted to a string: `()` and `false` produce an **empty
  string** (so `{{ maybe_null }}` never prints `()`), everything else goes
  through `to_string()`.
- It is always escaped: `&` `<` `>` `"` `'` → HTML entities. Attributes,
  `<script>` and `<style>` follow different rules — see 2.5.

**Whitespace trimming:** `{{-` removes whitespace and newlines on the left,
`-}}` on the right.

```html
<td>
  {{- total -}}
</td>
```
→ `<td>1500</td>`

### 2.2 Raw output

```html
{{ raw(article.html) }}
```

`raw(s)` returns a value of type `Html`, which is not escaped. This is the only
way to insert ready-made HTML. Values of type `Html` (the result of `raw()`, a
rendered component) are not escaped a second time.

### 2.3 Comments

| Form | Behaviour |
|---|---|
| `{{! text }}` | rhaix comment, **never** reaches the output |
| `<!-- text -->` | ordinary HTML comment, sent to the client |

### 2.4 Literal braces

```html
<p>{{ "{{" }} is not interpolation {{ "}}" }}</p>
```

Or as a block:

```html
<rhaix:raw>
  everything in here is emitted verbatim: {{ this is not an expression }}
</rhaix:raw>
```

### 2.5 Output context (security)

Escaping depends on **where** the `{{ }}` sits. This is not a setting — the core
determines the context at compile time and applies the matching rule.

| Context | Rule |
|---|---|
| text in markup | HTML-escape `& < > " '` |
| attribute value | attribute escaping plus the rules below |
| `href` `src` `action` `formaction` `xlink:href` `poster` `data` | additionally, a URL scheme check |
| `on*` (`onclick`, `onerror`, …) | **expressions are forbidden**; only a static string written by the author |
| `<script>` | only `{{ json(x) }}`; a bare expression is a compile error |
| `<style>` | interpolation is **forbidden** |
| `{{ raw(x) }}` | no escaping — the author's responsibility |

**URL schemes.** Allowed: relative paths, `http`, `https`, `mailto`, `tel`,
`ftp`, `data:image/*`. Anything else (above all `javascript:`, `vbscript:`,
`data:text/html`) is replaced with `#` and logged. This is what saves the classic
`<a href={row.link}>` where `link` came from the database, written by a user.

**Event handlers.** `onclick={expr}` is a compile error. `on*` keys arriving via
`@attr` or `{...spread}` are dropped at render time with a warning in the log:
the map is built at runtime, so it cannot be checked in advance. A static
`onclick="alert(1)"` written directly in the markup works as always.

**Any** attribute whose name starts with `on` is treated as a handler: no
standard HTML attribute with such a name is anything else, and being wrong here
costs more than being cautious.

**Data for JS.** The only way to pass a value into a script:

```html
<script>
  const todo = {{ json(todo) }};       // JSON plus escaping of `<`, U+2028/2029
  const ids  = {{ json(ids) }};
</script>
```

`{{ todo.title }}` inside `<script>` is a compile error with the hint "use
`json()`". That closes the XSS hole which HTML escaping does not catch, because
inside a JS string it does nothing.

---

## 3. Attributes

```
attribute    = directive | interpolated | expression | boolean | spread ;

interpolated = name , "=" , '"' , { text | interp } , '"' ;
expression   = name , "=" , "{" , rhai-expr , "}" ;
boolean      = name ;
spread       = "{" , "..." , rhai-expr , "}" ;
directive    = "@" , name , [ "=" , "{" , rhai-expr , "}" ] ;
```

```html
<a href="/todo/{{ t.id }}?tab={{ tab }}">…</a>   <!-- interpolation in a string -->
<a href={link}>…</a>                              <!-- expression -->
<input required>                                  <!-- boolean -->
<input {...field_attrs}>                          <!-- spread a map -->
```

**Expression value → attribute:**

| Expression result | HTML |
|---|---|
| `"abc"`, `42` | `attr="abc"` / `attr="42"` |
| `true` | `attr` (no value) |
| `false` or `()` | the attribute is **not emitted** |
| array | values joined with spaces |
| map | `key="value"` pairs (for `@attr`) |

The value is always escaped for the attribute context. Quoted attributes may
contain `{{ }}`; unquoted ones only `{ }`.

`href`/`src`/`action`/`formaction` get the URL scheme check, and `on*` attributes
cannot be the result of an expression or a `{...spread}` — see 2.5.

---

## 4. Directives

Directives are handled by the core and never reach the output.

### 4.1 `@if` / `@else-if` / `@else`

```html
<p @if={user.is_admin}>Admin</p>
<p @else-if={user.is_editor}>Editor</p>
<p @else>Guest</p>
```

- `@else-if` / `@else` must be on the **adjacent element** after `@if` (only
  whitespace and comments may sit between them). Otherwise it is a compile error.
- The condition is a Rhai expression; truthiness: `false`, `()`, `0`, `""`, an
  empty array or map count as false (deliberately softer than Rhai, because a
  template works with data coming from a database).
- To hide a group of elements without a wrapper, use `<template @if={...}>`.

### 4.2 `@for`

```html
<li @for={todo in todos}>{{ todo.title }}</li>
<li @for={(todo, i) in todos}>{{ i + 1 }}. {{ todo.title }}</li>
<option @for={(v, k) in options} value={k}>{{ v }}</option>   <!-- over a map -->
<span @for={n in 1..=5}>{{ n }}</span>                        <!-- over a range -->
```

Inside, an `iter` object is available:

| Field | Value |
|---|---|
| `iter.index` | 0-based index |
| `iter.number` | 1-based |
| `iter.first` / `iter.last` | `bool` |
| `iter.count` | length of the collection |

Why `iter` and not `loop`: `loop` is a Rhai keyword, and `{{ loop.number }}`
simply does not compile. Found while implementing M1.

The counter map is built only when `iter` is actually mentioned in the loop body
— on a thousand-row table that is measurable.

`@key={expr}` is optional. In v1 it does not affect the output; it is reserved
for morph swaps (idiomorph) and will be emitted as `data-rhx-key` once those are
enabled.

**Order with `@if`:** `@for` is the outer one, `@if` is evaluated **on every
iteration** (so the loop variable may be used in the condition):

```html
<li @for={t in todos} @if={!t.done}>{{ t.title }}</li>
```

To check a condition **before** the loop, wrap it:

```html
<template @if={!todos.is_empty()}>
  <li @for={t in todos}>…</li>
</template>
```

### 4.3 `@class`

Merged with the static `class`, it does not replace it.

```html
<li class="todo-item" @class={#{"completed": t.done, "urgent": t.priority > 5}}>
<li @class={["a", "b"]}>
<li @class={some_string}>
```

### 4.4 `@style`

```html
<div @style={#{"width": pct + "%", "color": c}}>
```

`()`/`false` as a value skips that property.

### 4.5 `@attr`

A dynamic set of attributes from a single map.

```html
<input @attr={#{"checked": t.done, "disabled": locked, "value": t.title}}>
```

`on*` keys in the map are a compile error (if written as a literal) or dropped at
runtime with a warning (if the map was built dynamically). See 2.5.

### 4.6 `@html` / `@text`

Replace the element's content (the element must have no children in the markup).

```html
<div @html={post.body_html}></div>        <!-- raw HTML -->
<div @text={user_input}></div>            <!-- escaped text -->
```

### 4.7 `@oob` — a fragment outside the main target

HTMX can replace more than what the client asked for: an element carrying
`hx-swap-oob` replaces its counterpart on the page. This is an everyday scenario
for internal tools — you saved something in a modal, and the table row behind it
needs updating.

```html
<tr @oob={"#row-" + order.id}>
  <td>{{ order.id }}</td><td>{{ order.title }}</td>
</tr>
```

→ `<tr id="row-42" hx-swap-oob="outerHTML:#row-42">…</tr>`

The core sets `hx-swap-oob` itself and, when the selector looks like `#id`, the
`id` as well. The swap mode is given by a second value:
`@oob={["#list", "beforeend"]}`. A map here is a mistake and produces a warning —
the directive expects a selector, not a set of options.

`@oob` does not work on a component (4.8) — wrap it in an element:

```html
<div @oob={"#nav"}><Nav /></div>
```

The typical case is persistent chrome outside `<main>` (a nav menu, a header
counter) that the fragment rule (6.3) never re-renders on htmx navigation,
because it lives in the layout and the layout doesn't run on that path at all.
The component that draws it recomputes its state every time (it sees
`req.path` the same way a page does), and `@if={req.is_htmx}` on the wrapper
keeps it from duplicating on a full load, where the layout just drew the same
block. The recipe is `components/Nav.rhx` in `examples/demo` and
`examples/cookbook`.

### 4.8 Summary table

| Directive | On what | Value |
|---|---|---|
| `@if` `@else-if` `@else` | any element, `<template>`, component | expression / — |
| `@for` | the same | `x in coll`, `(x, i) in coll` |
| `@key` | together with `@for` | expression |
| `@class` `@style` `@attr` | HTML elements | map / array / string |
| `@html` `@text` | HTML elements | expression |
| `@oob` | HTML elements | selector expression (4.7) |

Directives on a component: `@if/@else*/@for/@key` are allowed. `@class`/
`@style`/`@attr`/`@html`/`@text`/`@oob` on a component are a compile error — a
component decides its own markup, so pass props or wrap it in an element.

---

## 5. Components

### 5.1 Naming and resolution

```html
<TodoItem />              → components/TodoItem.rhx
<Ui.Button />             → components/ui/Button.rhx
<Forms.Field.Text />      → components/forms/field/Text.rhx
```

- A tag is treated as a component if it starts with a capital Latin letter.
- A dot separates directories; the segment after the last dot is the file name
  (case as in the tag), directory segments are lowercased.
- If the file is not found it is a compile error with the closest name
  suggested: `component "TodoItm" not found; did you mean "TodoItem"?`
- Circular dependencies (`A → B → A`) are detected at compile time, not at
  runtime.

### 5.2 Props

```html
<TodoItem todo={t} editable compact="yes" {...rest} />
```

Inside the component:

```
---
let todo     = props.todo;                  // required; absent → ()
let editable = props.editable ?? false;     // default value
let compact  = props.compact ?? "no";
---
```

- `props` is a map of everything that was passed.
- For convenience every prop is also available as a **variable of the same
  name** (so `todo` works without `let todo = props.todo`), but the explicit
  declaration is recommended — it documents the component.
- Names containing hyphens are only reachable through `props["data-x"]`.
- Validation is ordinary code:

```
---
if props.todo == () { throw "TodoItem: required prop `todo` is missing"; }
---
```

A `throw` inside a component produces an error with the chain:
`pages/todo.rhx:12 → TodoList.rhx:4 → TodoItem.rhx:2`.

### 5.3 Scope isolation

A component **does not see** the caller's variables — only `props`, the global
objects (`req`, `db`, `page`, …) and its own frontmatter. This is deliberate: it
keeps the component portable and its errors local.

### 5.4 Slots

```html
<!-- components/Card.rhx -->
<div class="card">
  <header @if={slots.has("header")}><slot name="header"/></header>
  <div class="card-body"><slot>Empty</slot></div>
  <footer><slot name="footer"/></footer>
</div>
```

```html
<Card>
  <template slot="header"><h2>{{ title }}</h2></template>
  <p>Main content</p>
  <button slot="footer">OK</button>
</Card>
```

- `<slot/>` without a name is the default content; `<slot name="x"/>` is named.
- Content inside `<slot>…</slot>` is the fallback used when the slot is not
  passed.
- A slot is rendered in the **caller's scope** (it sees the caller's variables,
  not the component's).
- `slots.has("name")` → `bool`.
- Scoped slots (passing data back out of a slot) are **[v1.1]**.
- A slot counts as passed when its content is non-empty: `<Card></Card>` shows
  the fallback rather than a blank.

### 5.5 Grouping tags

| Tag | Role |
|---|---|
| `<template>` | carries directives, is not emitted itself |

`<template @if={...}>` or `<template @for={...}>` is the only way to attach a
directive to a group of elements without an extra HTML wrapper. There is no
separate `<Fragment>`: a second spelling for the same thing only adds something
to remember.

---

## 6. Layout and pages

### 6.1 Layout

`layouts/main.rhx` is the only place with a full `<html>`. The page lands in
`<slot/>`.

```html
---
let nav = [#{href:"/", t:"Home"}, #{href:"/todo", t:"ToDo"}];
---
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>{{ page.title ?? "rhaix" }}</title>
  <rhaix:head />
</head>
<body hx-boost="true">
  <nav>
    <a @for={i in nav} href={i.href} hx-target="#main" hx-push-url="true">{{ i.t }}</a>
  </nav>
  <main id="main"><slot /></main>
  <rhaix:scripts />
</body>
</html>
```

| Special tag | What it inserts |
|---|---|
| `<rhaix:head/>` | `public/**.css`, hoisted component `<style>`, **and the scripts**: htmx, the `rhaix.js` core, the `ui.js` interface (7.7), `public/**.js` with `defer` |
| `<rhaix:scripts/>` | hoisted component `<script>` only |
| `<rhaix:csrf/>` | a hidden CSRF field, when the automatic one cannot be placed (7.2) |
| `<rhaix:raw>…</rhaix:raw>` | a block emitted without processing |

Scripts go in `<head>`, not at the end of `<body>`, on purpose: htmx only swaps
`<body>`, and a script from there would run again on every boosted navigation
(before 1.2.5 it did — each such navigation added another listener, and toasts
multiplied). Project scripts get `defer`: they run once, when `<body>` already
exists. A layout without `<rhaix:head/>` still works — everything then goes into
`<rhaix:scripts/>` as before.

### 6.2 Choosing a layout

```
---
page.layout = "admin";     // layouts/admin.rhx
page.layout = false;       // no layout at all, even on a full load
---
```

The default is `layouts/main.rhx` when it exists. The wrapper is selected through
the same `page` map as `page.title`: there is deliberately no separate `layout()`
function — one way instead of two.

Names may contain letters, digits, `-` and `_`. Anything else is ignored with a
warning: the value may come from a form, and `page.layout = "../../etc/passwd"`
must not lead anywhere.

### 6.3 The fragment rule

- A normal visit → page plus layout.
- An explicit `hx-get`/`hx-post` (a request with `HX-Request`) → only the
  page's or partial's own markup, no layout. In a script: `req.is_htmx`.
- A **boosted** request (an ordinary link or form under `<body hx-boost>`,
  header `HX-Boosted`) and a **history restore** (`HX-History-Restore-Request`)
  → page plus layout. htmx swaps such a response into the **whole `<body>`**: a
  fragment without the layout would wipe the nav, `#main` and the toast
  container. `req.is_htmx` is false here and `req.is_boosted` is true.
  Component styles travel inside `<body>` in such a response — htmx leaves
  `<head>` alone.

This is automatic — there is nothing to switch on. Every response carries
`Vary: HX-Request, HX-Boosted, HX-History-Restore-Request`.

Consequence: **the layout does not run on htmx navigation (explicit `hx-get`).** Everything
in it — a nav menu, a footer, a header counter — stays exactly as the first
full render drew it, until something updates those spots itself.

- **The framework handles `<title>` for you.** If the page set `page.title`,
  the fragment response carries a `<title>` tag ahead of the markup — htmx
  picks it up from anywhere in the response body, regardless of whether it's
  inside the swap target, and updates the tab. The value is used **as-is**,
  without whatever formatting the layout applies (`"{{ brand }} — {{ page.title }}"`
  may be its own thing there): if you want that exact shape in the tab, build
  the full string into `page.title` yourself. A page that never touched
  `page.title` gets no tag at all — the title stays whatever the previous page
  showed.
- **The rest of the persistent chrome — menus, counters — is the project's
  job**, via `@oob` (4.7). A working recipe is `components/Nav.rhx` in
  `examples/demo` and `examples/cookbook`.

### 6.4 Routes

| File | Route | Parameter access |
|---|---|---|
| `pages/index.rhx` | `/` | — |
| `pages/todo.rhx` | `/todo` | — |
| `pages/todo/[id].rhx` | `/todo/:id` | `req.param("id")` |
| `pages/blog/[...slug].rhx` | `/blog/*` | `req.param("slug")` |
| `partials/Stats.rhx` | `/components/stats` | — |

Routes are defined by the **directory structure only**. The `route()` and
`methods()` declarations that appeared in the draft spec were removed: reading
them would require executing the frontmatter before knowing the route.

### 6.5 Method-specific logic and early exit

```
---
if req.method == "POST" {
  db.exec("insert into todos(title) values(?)", [req.form("title")]);
  hx.toast("Added", "success");
  hx.trigger("todoChanged");
}
if req.method == "DELETE" {
  db.exec("delete from todos where id=?", [req.query("id")]);
  return "";                     // empty response — htmx removes the element
}
if session.user == () {
  res.redirect("/login");
  return;                        // the markup is not rendered
}
let todos = db.query("select * from todos order by id");
---
<TodoList todos={todos} />
```

- `return;` — respond without rendering the markup (for a redirect or a 204).
- `return <string>` — return exactly that string (no escaping is applied; the
  content is treated as ready HTML).
- Rendering is cancelled only by `res.redirect`, `hx.redirect` and `hx.refresh`.
  `res.status(422)` changes the status but still renders the page — which is
  exactly what a form with validation errors needs.
- Only an **explicit `return`** becomes the body. The value of the last
  expression does not: in Rhai it survives a trailing semicolon, so frontmatter
  ending in `db.insert(...);` used to silently send the new id to the client
  instead of the page.

### 6.6 `middleware.rhx`

A file in the project root that runs **before** the page — for everything that
would otherwise be duplicated in every protected file:

```
---
if req.path.starts_with("/admin") && session.user == () {
    hx.toast("Please sign in", "error");
    res.redirect("/login");
}
---
```

- It sees the same globals as a page and shares the same `res`/`hx`.
- `res.redirect` / `hx.redirect` / `hx.refresh` stop processing: the page never
  runs.
- If the middleware returns a value, that value becomes the response body and
  the page is not executed.
- CSRF is verified **before** the middleware, so a forged request never reaches
  a single line of application logic.

**Roles and permissions — a recipe.** The framework does not impose its own
user model, but everything needed is already there. A working example lives in
`examples/cookbook` (`middleware.rhx`, `scripts/access.rhai`, `pages/roles.rhx`,
`pages/reports.rhx`).

```rhai
// scripts/access.rhai — one permission table for the whole app
fn permissions(role) {
    switch role {
        "admin"   => ["orders.view", "orders.export", "orders.cancel"],
        "manager" => ["orders.view", "orders.export"],
        _         => ["orders.view"],
    }
}
fn can(user, permission) {
    if user == () { return false; }
    permissions(user.role).contains(permission)
}
```

```rhai
// middleware.rhx — who is this, and where may they go
let id = session.get("user_id");
page.user = if id == () { () } else { db.get("users", id) };

let rules = [["/reports/export", "orders.export"], ["/reports", "orders.view"]];
for rule in rules {
    if req.path != rule[0] && !req.path.starts_with(rule[0] + "/") { continue; }
    if page.user == () {
        res.redirect(url("/login", #{ next: req.url }));
    } else if !can(page.user, rule[1]) {
        hx.toast("Access denied", "error");
        res.redirect("/");
    }
    break;
}
```

```html
<!-- any page: page.user is already there, no extra query -->
<button @if={can(page.user, "orders.cancel")} hx-post="/orders/cancel">Cancel</button>
```

Four things such schemes usually get wrong:

- **Pages check a permission, not a role.** Write `can(user, "orders.export")`,
  not `user.role == "admin"`. A new role is then one line in `permissions`,
  not a hunt through every file.
- **The session holds only the id; the role comes from the database.** A role
  change or a ban then applies on the very next request. A role stored in the
  cookie would live until the session ends.
- **Hiding a button is not forbidding the action.** The middleware guards
  paths, not methods and parameters. So the action handler checks `can(...)`
  again: a hidden button is easy to recreate by hand.
- **Refusing an htmx button:** `res.status(403); hx.toast(...); hx.reswap("none");
  return "";`. Without `reswap("none")` htmx would replace the target with
  nothing.

`page` is the same value in the middleware and on the page, so the page sees
`page.user` without another database query. `req.url` is the path together
with the query string — exactly what "send them back after login" needs. When
redirecting to `next`, check that it starts with `/` but not `//`: otherwise
`?next=//evil.com` takes the user to someone else's site.

### 6.7 `api/` — JSON instead of HTML

`api/` is a third top-level directory next to `pages/` and `partials/`.
`api/orders.rhx` becomes `/api/orders`, `api/orders/[id].rhx` becomes
`/api/orders/{id}`. Segment rules are identical to `pages/`.

```
api/orders.rhx
---
if req.method == "POST" {
    let sent = req.json();
    let errors = validate(sent, #{ customer: "required|min:2" });
    if !errors.is_empty() { res.status(422); return #{ errors: errors }; }
    res.status(201);
    return db.get("orders", db.insert("orders", sent));
}

return #{ data: db.find("orders") };
---
```

| | `pages/` | `api/` |
|---|---|---|
| Content type | `text/html` | `application/json` |
| Layout | applied | never |
| Returned map or array | text as-is | serialised to JSON |
| Errors, 404, diagnostics | HTML page | `{"error": "..."}` |
| Session | yes | **no** |
| CSRF | verified | not verified |

**A returned map becomes JSON by itself.** Otherwise `return #{ ok: 1 }` would
emit Rhai-shaped `#{"ok": 1}` — close enough to JSON to pass a human's eye, far
enough to break a parser. A returned string is left alone: either `json_encode()`
already built it, or it is deliberately not JSON. An empty string plus
`res.status(204)` is an ordinary no-body reply.

**There is no session, on purpose.** If a route that skips CSRF still read
cookies, `POST /api/delete` from another site would run as the logged-in user.
So `api/` reads no cookie and sets none; the only way to identify yourself is a
header token, which another site cannot attach. If you want JSON **with** a
session and CSRF, use a normal page: `page.layout = false` plus
`res.header("content-type", ...)` gives exactly that.

**The body arrives via `req.json()`**, not `req.all_form()`; it is `()` when the
body is not JSON.

Authentication belongs in `middleware.rhx` — one place for the whole API:

```
middleware.rhx
---
if req.path.starts_with("/api/") {
    let sent = req.header("authorization") ?? "";
    let token = if sent.starts_with("Bearer ") { sent.sub_string(7).trim() } else { "" };
    let owner = if token == "" { () } else { db.find("api_tokens", #{ hash: sha256(token) }) };
    if owner == () || owner.is_empty() {
        res.status(401);
        return #{ error: "a valid token is required" };
    }
}
---
```

A worked recipe with validation, partial update and mass-assignment protection
lives in `examples/cookbook/api/`.

**CORS — `[api] cors`.** If the API is called by a frontend on another domain:

```toml
[api]
cors = ["https://app.example.com"]   # or "*" — any site
```

The permission applies to `/api/…` only. The framework answers the preflight
(`OPTIONS`) itself before any script runs, adds `Access-Control-Allow-Origin`
to every response (errors and 404 included) and `Vary: Origin`. Pages never get
CORS: they live on the cookie session, and letting another site read them would
hand it everything a signed-in user sees. `"*"` is safe in `api/` precisely
because there are no cookies there. Your own frontend on the same domain or a
mobile client does not need CORS.

**API description — OpenAPI 3.1 from the files themselves.**

```
rhaix openapi                  # to the console
rhaix openapi -o openapi.json  # to a file
```

No annotations to write: the description comes from what is already in
`api/*.rhx`:

| From | In the spec |
|---|---|
| file name `api/orders/[id].rhx` | path `/api/orders/{id}` and parameter `id` |
| `if req.method == "POST" { … }` | a POST operation; the rest of the file is GET |
| `if req.method != "POST" { res.status(405) … }` | the file is POST-only |
| `req.query_int("page")`, `req.query("q")` | typed query parameters |
| `req.json()` + `validate(sent, #{ … })` | the JSON body: fields, required, `email`, bounds, `enum` |
| `res.status(422)` in a branch | possible response codes of that operation |
| a comment at the top of the frontmatter | `summary` (first sentence) and `description` |
| `middleware.rhx` reads `Authorization` for `/api` | a bearer scheme and 401/429 on every operation |

It is a heuristic: what cannot be derived is simply absent, nothing is made
up. To serve the spec live (for Swagger UI or client generators), set
`[api] openapi = true`: it then appears at `/api/openapi.json`. In dev it is
rebuilt on every request, in production once at startup. `[api] title` and
`version` go into `info`.

---

## 7. Scope and global objects

| Name | Where available | What it is |
|---|---|---|
| frontmatter variables | this file | `let x = …` |
| `props` | component / partial | the values passed in |
| `slots` | component | `slots.has(name)` |
| `iter` | inside `@for` | iteration counters |
| `page` | everywhere | shared page map: `page.title`, `page.layout` |
| `req` `res` `hx` | everywhere | request / response / HTMX |
| `state` | everywhere | process-wide store: `state.get/set/has/remove`; rate limiting: `state.allow/retry_after/reset` (7.2) |
| `live` | everywhere | `live.send(topic[, detail])` — notify open pages (7.8) |
| `db` | everywhere | database: `query/one/exec/tx` and `find/get/count/insert/update/delete` |
| `session` | everywhere | signed cookie: `get/set/has/remove/clear/all`, `session.user` (7.2) |
| `csrf` | everywhere | `csrf.token` (7.2) |
| `http` | everywhere | calling other services (7.3) |
| `log` | everywhere | `log.info/warn/error` |
| helpers | everywhere | `url()`, `now()`, `date()`, `slug()`, `money()`, `json()`, … (7.4) |

**`url(path, params)`** is the only correct way to build a link that carries
state:

```html
<a href={url("/orders", #{ q: q, sort: sort, page: n })}>{{ n }}</a>
```

Parameters are encoded, `()` and empty strings are dropped. Manual concatenation
(`"/orders?q=" + q`) breaks on an `&` inside a value — it is deliberately absent
from the documentation.

**Typed request access.** `req.query()` and `req.form()` return strings, so for
numbers and flags there are `req.query_int/query_float/query_bool` and
`req.form_int/form_float/form_bool`. Each returns `()` when the value is missing
or does not parse — followed by the usual `?? 1`.

**Shared functions — `scripts/*.rhai`.** Nothing has to be imported: everything
declared there is visible from any `.rhx`, both in frontmatter and in `{{ }}`.

```rhai
// scripts/orders.rhai
fn status_label(status) {
    switch status { "new" => "New", "paid" => "Paid", _ => status }
}
```

```html
<!-- pages/table.rhx — no import anywhere -->
<span>{{ status_label(o.status) }}</span>
```

The files are loaded **once at startup**, so editing `scripts/*.rhai` requires
restarting `rhaix dev` — it says so in the console. A broken shared script stops
the start rather than the first request that uses it.

`import "helpers" as h;` also works (modules are read from the same directory and
cannot escape it), but it is rarely needed: an imported module lives only within
one block of code, so it does **not** survive from frontmatter into `{{ }}`.

### 7.1 Two Rhai facts worth knowing

1. **`loop` is a keyword**, which is why loop counters live in `iter` (4.2).
2. **Many string methods work in place.** Rhai's own `trim()` trims the string
   and returns `()`, which made `let title = s.trim();` produce an empty value.
   rhaix overrides `trim()`: it still trims in place but also returns the
   result, so both spellings do what you expect.
3. **SQLite has no boolean type.** `done` arrives as `0`/`1`. In a template that
   does not matter (`@if={t.done}` uses rhaix truthiness, and `!t.done` works too
   — negation is overridden by the same rule). But `if t.done` in frontmatter is
   plain Rhai, which demands an actual `bool`, so write `if bool(t.done)`.

### 7.2 Sessions and CSRF

```rhai
session.set("user", name);
session.user                    // shorthand for session.get("user")
session.get("cart") ?? []
session.has("user")
session.remove("cart")
session.clear();                // sign out
```

A session is a **signed cookie**, not a server-side record. The consequences are
worth knowing up front:

- The state travels to the client and back, so the app can run as two copies
  behind a load balancer with nothing shared between them.
- The client **can read** the contents (base64 is not encryption) but cannot
  change them: the HMAC-SHA256 signature would not match and the session would
  simply become empty. Passwords and anything secret do not go in there.
- The size limit is about 4 KB. Keep identifiers in the session and data in the
  database.
- `session.clear()` expires the cookie **in that browser**. An already-issued
  cookie cannot be revoked on the server — you can only wait for it to expire or
  change the secret.
- `Set-Cookie` appears when the session was actually modified. A page that
  writes nothing carries no cookie at all.
- **A session stays alive while it is used.** Its lifetime (`session_days`)
  counts from the last time the cookie was issued. Once half of it has passed,
  any request reissues the cookie for the full lifetime — even if nothing in the
  session changed. An active user is never logged out mid-work; an inactive one
  is, `session_days` after their last visit.
- A response carrying `Set-Cookie` gets `Cache-Control: private, no-store` unless
  the script set caching itself: a shared cache (CDN, proxy) must not hand one
  visitor's session to the next.

**CSRF works by itself.** A form that changes data gets a hidden field added:

```html
<form method="post" action="/login">   <!-- or hx-post="/login" -->
  <input name="name">
</form>
```

```html
<!-- in the response -->
<form method="post" action="/login"><input type="hidden" name="_csrf" value="…">
  <input name="name">
</form>
```

An element that changes data **without a form** gets a header instead:

```html
<button hx-delete={url("/todo", #{ id: t.id })}>×</button>
<!-- → hx-headers='{"x-csrf-token": "…"}' -->
```

A request whose method is not `GET`/`HEAD`/`OPTIONS` and which carries no valid
token does not reach `middleware.rhx` at all — the response is `403` with the
text "This form has expired. Reload the page and try again."

| Case | What to do |
|---|---|
| `method` is an expression (`method={m}`) | place `<rhaix:csrf />` inside the form by hand |
| you set your own `hx-headers` | add the token there: `#{"x-csrf-token": csrf.token}` |
| the form posts to another site | `data-no-csrf` on the form |
| a public webhook that accepts POST | `csrf = false` in `[app]` — disables the check **for the whole app** |

The token is **lazy**: it is created by the first form on the page. A page with
no forms gets neither a token nor a cookie.

Configuration lives in the `[app]` section of `rhaix.toml`:

```toml
[app]
# The signing key. Better here than nowhere, but best in RHAIX_SECRET.
secret = "…"
csrf = true            # on by default
session_cookie = "rhaix_session"
session_days = 30
tz_offset = "+03:00"   # which offset dates are displayed in
http_timeout = 10      # seconds
```

The signing key is looked up in this order: `RHAIX_SECRET` → `[app] secret` →
a `.rhaix-secret` file (created by `rhaix dev`, never committed) → a random one
for the lifetime of the process. The last option works, but after a restart every
session becomes invalid — in production that is reported as a warning in the log.

**Rate limiting — `state.allow`.** Password guessing, form spam, an overly
chatty API client — all of it is "no more than N times per T seconds":

```rhai
// pages/login.rhx
let attempts = `login:${req.ip}:${username.to_lower()}`;
if !state.allow(attempts, 5, 300) {          // 5 attempts per 5 minutes
    error = `Too many attempts. Try again in ${state.retry_after(attempts)} s`;
    res.status(429);
} else if verify_password(password, user.password) {
    state.reset(attempts);                   // a successful login starts over
    // …
}
```

| Call | What it does |
|---|---|
| `state.allow(key, max, seconds)` | counts one attempt; `true` if it fits the limit |
| `state.retry_after(key)` | seconds until the window resets — for the message and `Retry-After` |
| `state.reset(key)` | forget the attempts under this key |

The key is any string, so a limit can hang on an address, a user, an API token
or a combination. For a login the key must include **the user name too**:
otherwise an attacker logs into their own account, `state.reset` clears the
counter for their address, and guessing someone else's password carries on.
Counters live in process memory: two copies behind a load balancer count
separately, and a restart clears them — enough to stop password guessing.

**`req.ip`** is the client address. Behind your own reverse proxy (nginx,
Caddy) it is always the proxy's address, so turn this on there:

```toml
[server]
trust_proxy = true   # req.ip comes from the last X-Forwarded-For entry
```

Without the flag `X-Forwarded-For` is ignored entirely: anyone can send it, and
a limit would be one header away from useless. Turn it on **only** when the app
cannot be reached except through your proxy.

### 7.3 `http` — calling other services

```rhai
let nbu = http.get("https://bank.gov.ua/…?valcode=EUR&json");
let euro = if nbu.ok && nbu.json.len() > 0 { nbu.json[0] } else { () };
```

```rhai
http.post(url, #{ title: title })                    // map → JSON
http.post(url, "a=1&b=2", #{ "Content-Type": "application/x-www-form-urlencoded" })
http.get(url, #{ Authorization: `Bearer ${token}` })
http.put(url, body) / http.patch(...) / http.delete(url)
```

The call is synchronous: no `await`. The response is a map:

| Field | What it is |
|---|---|
| `status` | response code; `0` if the server was never reached |
| `ok` | `true` for 2xx |
| `body` | the body as text |
| `json` | the parsed body, or `()` if it is not JSON |
| `headers` | a map of headers (names lowercased) |
| `error` | the error text, or `()` |

**A network failure does not break the page**: when the API is down you get
`ok: false` and `error`, not a 500. That is why none of the examples use `try`.

Two things worth remembering:

- The time spent waiting is **not** counted against the script budget, but the
  page still waits. A slow API means a slow page; `http_timeout` in `[app]`
  bounds the wait.
- Only `http://` and `https://` addresses are accepted. If the address comes from
  a user, check it yourself: the server will go there from its side of the
  network.

### 7.4 Dates, strings, money

```rhai
now()                      // seconds since the epoch
today()                    // "2026-09-17"
date(t.created)            // "2026-09-17" — understands a timestamp and a database string
date(t.created, "DD.MM.YYYY 'at' HH:mm")
datetime(now())            // "2026-09-17 14:05:09"
timestamp("2026-09-17T14:05:09Z")
now() + days(7)            // also hours(), minutes()
```

Date patterns use tokens that read without a reference card:
`YYYY YY MM M DD D HH H mm m ss s`. Text in double quotes is kept verbatim.
Everything else is just characters.

Time is stored in UTC internally; display is shifted by `tz_offset` from `[app]`.
There is no daylight-saving handling — if you need real time zones, take them
from the database.

Date parsing is strict: `2026-02-31`, `2026-02-29` (not a leap year),
`2026-09-17 25:00` or `12:60` are not dates. `timestamp(...)` returns `()`,
`date(...)` an empty string, and the `date` rule in `validate()` rejects such a
form. Before 1.5.0 an extra day quietly rolled into the next month and a
garbled time became zero.

```rhai
slug("Привіт, світе!")     // "pryvit-svite" — transliteration per Ukrainian standard
cut(text, 20)              // trim at a word boundary and add "…"
strip_tags(text)
capitalize(text)
money(1234.5)              // "1 234,50" (non-breaking space, rounds away from zero)
money(value, 0)
uuid() / random_id() / sha256(text)
hash_password(pw) / verify_password(pw, hash)   // Argon2id, salt inside the hash
json_encode(value) / json_decode(text)
is_blank(value)            // (), "", "   ", [], #{}
```

`json()` and `json_encode()` are different things: the first escapes whatever
could close a tag and is meant for `<script>` (2.5); the second gives a plain
string for a database or an API.

**Passwords — `hash_password(pw)` and `verify_password(pw, hash)`.** Argon2id
with OWASP defaults (19 MiB, 2 passes) and a random salt. The database stores a
PHC string (`$argon2id$…`), so no separate salt column is needed. `sha256()` is
for checksums, **not** passwords: a fast hash is brute-forced billions per
second. `hash_password("")` returns `()`, and `verify_password` on a broken hash
returns `false` rather than erroring.

---

### 7.5 Batteries: validation, pagination, uploads, CSV, mail

**`validate(values, rules)`** — validate a form in one call instead of a dozen
`if`s. Returns a map of `field → message` (empty when valid):

```rhai
let errors = validate(req.all_form(), #{
    name:  "required|min:2",
    email: "required|email",
    age:   "int|between:18,120",
    again: "same:pass",
    role:  "in:user,admin",
});
if !errors.is_empty() { res.status(422); }
```

Rules: `required email url int number min:N max:N between:a,b same:field
in:a,b,c`. `min`/`max`/`between` compare by **value** on an `int`/`number`
field, by string **length** otherwise. An empty optional field skips the rest.

**`paginate(total, per_page, page)`** — all the page arithmetic. `page` is
clamped, so `?page=999` gives the last page:

```rhai
let p = paginate(db.count("orders"), 20, req.query_int("page") ?? 1);
let rows = db.find("orders", #{}, #{ sort: "id desc", limit: 20, skip: p.skip });
```

Fields: `page pages per_page total skip from to has_prev has_next prev next
first last window`.

**`db.grid(table, req, options)`** — an admin table in one call: sorting by
column, filters, pages, all of it living in the URL.

```rhai
let g = db.grid("orders", req, #{
    sort:     ["id", "customer", "amount", "created"],  // what may be sorted by
    order:    "id desc",                                // when the URL says nothing
    filters:  #{ status: "eq", customer: "contains", amount: "between" },
    per_page: 20,
    where:    #{ owner_id: page.user.id },              // a bound the URL cannot override
});
```

```html
<th><a href={g.sort_url.amount}>Amount {{ g.arrow.amount }}</a></th>
<tr @for={o in g.rows}>…</tr>
<a @for={n in g.page.window} href={g.page_url[`${n}`]}>{{ n }}</a>
<input name="customer" value={g.values.customer}>
```

Returns: `rows`, `page` (the same as `paginate()`), `total`, `sort`, `dir`,
`values` (current filter values for form fields), `sort_url` and `arrow`
(link and ▲/▼ for each column in `sort`), `page_url` (keyed by page number as
a string), `prev_url`, `next_url`, `reset_url`, `filtered`.

- **The sort column from the URL is checked against `sort`.** A foreign
  column or `id;drop table` is simply ignored: otherwise it would be an
  injection into `order by`.
- **Links keep all the state.** Sorting keeps the filter, the filter keeps
  the sort, and the page resets to the first.
- **Operators:** `eq ne contains starts ends gt gte lt lte between`.
  `between` reads `field_from` and `field_to`. Numbers from the URL are
  compared as numbers.
- **`where` is applied last and wins:** `?owner_id=7` from the URL cannot
  override `where: #{ owner_id: me }`.
- **An unknown option or operator is an error,** not silence.

The option is `order`, not `default`: that is a reserved word in Rhai. A
working recipe with live search that keeps focus is
`examples/cookbook/pages/grid.rhx`.

**`db.attach` and `db.attach_count` — related rows in one query.** The most
common reason for a slow page is a query per table row:
`db.get("companies", d.company_id)` inside `@for` is 25 queries instead of one.
On SQLite it barely shows; on PostgreSQL every query is a network round trip.
In a CRM built on rhaix the companies page, with three such queries per row,
took 195 ms, and 19 ms after `attach`.

```rhai
let deals = db.find("deals", #{}, #{ limit: 25 });
deals = db.attach(deals, "companies", "company_id", "company");      // d.company — a row or ()
let companies = db.find("companies", #{}, #{ limit: 25 });
companies = db.attach_count(companies, "deals", "company_id", "deals");  // c.deals — a number, 0 if none
```

```html
<td>{{ d.company.name ?? "-" }}</td>   <td>{{ c.deals }}</td>
```

`attach` fetches the rows with one `where id in (…)`, `attach_count` counts
with one `group by`. Table and column names are checked the same way as in
`db.find`.

**File uploads.** A form with `enctype="multipart/form-data"`:

```rhai
let f = req.file("photo");        // first upload or (); also req.files(name), req.has_file(name)
if f != () && f.is_image && f.size <= 2 * 1024 * 1024 {
    f.save(`public/uploads/${random_id()}.${f.extension}`);
}
```

Upload fields: `filename content_type size is_image extension`; methods `text()`
and `save(path)`. `save` checks the path — no absolute, no `..`, resolved
against the project root. The form's text fields stay in `req.form(...)`.

**Mail.** `mail.send(#{ to, subject, text, html? })`. Without a `[mail]`
section it runs in dev mode — logs the message instead of sending — so a contact
form works with no SMTP server. Real sending is behind the `mail` build feature
(which `rhaix build` enables when `[mail]` is present) plus the config section.

**CSV export — `csv()` and `res.download()`.**

```rhai
// pages/reports/export.rhx
let orders = db.find("orders", #{}, #{ sort: "id" });
res.download("orders.csv", csv(orders, #{
    columns: ["id", "customer", "amount"],
    titles:  ["No", "Customer", "Amount"],
    sep: ";", decimal: ",",      // what Excel expects in comma-decimal locales
}));
```

`csv(rows, options)` takes an array of maps (then `columns` is required: a Rhai
map does not remember field order) or an array of arrays. Options:

| Option | Default | What it does |
|---|---|---|
| `columns` | — | which fields, in which order |
| `titles` | `columns` | the header row |
| `sep` | `","` | the separator. Use `";"` for Excel in comma-decimal locales |
| `decimal` | `"."` | decimal mark for floats; `","` for that same Excel |
| `header` | `true` | whether to write the header row |
| `guard` | `true` | defuse cells that look like formulas |

Commas, quotes and newlines inside values are escaped per RFC 4180. Text that
starts with `=`, `+`, `-` or `@` gets a leading `'`. Otherwise
`=HYPERLINK(...)` in a customer name would run as a formula in the
accountant's spreadsheet. Numbers from the database are left alone. An unknown
option is an error, not silence.

`res.download(name, content)` sends a file instead of the page. The content is
a string or a blob. The type comes from the extension, or pass it as a third
argument: `res.download("a.bin", data, "application/x-foo")`. The framework does
the rest:

- **A file name with non-ASCII characters** reaches every browser:
  `filename*` per RFC 6266 plus an ASCII fallback. Path separators are
  stripped from the name.
- **A CSV string gets a BOM.** Without it Excel shows Cyrillic as mojibake.
- **Service headers:** `X-Content-Type-Options: nosniff` and
  `Cache-Control: private, no-store`.
- **A toast** (`hx.toast("Exported")`) waits for the next page in flash.
- **A link under `hx-boost`** works as expected. The server answers
  `HX-Redirect` to the same URL, and the browser downloads the file through a
  normal navigation while staying on the page. Otherwise htmx would paste the
  CSV into the page as text. The script therefore runs twice, so serve files on
  a GET that changes nothing. On an htmx POST the file cannot reach the user:
  the server logs a warning and adds `HX-Reswap: none` so at least the page is
  not damaged.

Recipes for all of them are in `examples/cookbook`.

### 7.6 Translations and markdown

**`t("key")`.** Files live in `locales/<lang>.toml` and are read through the
same `Files` abstraction as templates, so translations work in an embedded
binary too.

```toml
# locales/en.toml
greeting = "Hi"
items    = "{count} items in the cart"
```

```rhai
t("greeting")                  // "Hi"
t("items", #{ count: 3 })      // "3 items in the cart"
set_locale("uk");              // usually in middleware.rhx
locale()                       // current language
```

The default language is `[app] locale` (defaults to `uk`). Lookup order:
current language → default language → **the key itself**. A missing
translation shows up on the page as `todo.title`, not a blank spot. An
unknown language in `set_locale` is ignored with a warning, so `?lang=xx`
can't turn labels into raw keys.

**`markdown(text)`** returns `Html`, i.e. its output is not escaped — and the
text usually comes from a user. So there are two guardrails here, and they
are **not configurable**:

1. **raw HTML does not pass through** — `<img src=x onerror=…>` renders as
   text, not markup; only tags markdown itself generated reach the HTML;
2. **link schemes are checked** by the same rule as attributes:
   `[click](javascript:alert(1))` becomes `#`.

Because of this, no HTML sanitizer is needed — there is nowhere for anything
dangerous to come from. `markdown()` is enabled by the `markdown` build
feature (which `rhaix build` turns on when it's used).

### 7.7 Client: the core stays put, the interface is yours

The client side has two layers, and the boundary between them is deliberate:

| Layer | File | What it does | Replace it |
|---|---|---|---|
| core | `/_rhaix/rhaix.js` | component script registry, swapping non-2xx responses, style dedup | no |
| interface | `/_rhaix/ui.js` | toasts, modal `<dialog>` | yes — piece by piece or wholesale |

The core has not a single line about how anything looks. Everything visible
lives in `ui.js`, and a project can replace it without patching the framework.

**Responses with a status other than 2xx swap too.** htmx by default only
swaps `2xx` and silently drops the rest (`config.responseHandling`). In rhaix
`res.status(...)` is part of an ordinary response, not a signal that
something's wrong: `422` carries the same page with errors under the fields
(7.5), `403` explains an expired form, `404`/`500` carry a full page with
diagnostics. The core overrides this, so `validate()` on the server never
stays invisible on screen. `htmx:responseError` still fires for anyone
listening, and `204` does not swap — there is no body.

**Validation under fields is a component, not a directive.** The framework
deliberately adds no dedicated `<Field>` tag: `@if={errors.x}` plus a `<span>`
is already a sufficient primitive, and the wrapper is an ordinary project
component:

```
components/Field.rhx
---
let name  = props.name;
let label = props.label;
let value = props.value ?? "";
let error = props.error;
---
<p class="field">
  <label>{{ label }}<br>
    <input name={name} value={value} @class={#{"invalid": error != ()}}>
  </label>
  <span class="error" @if={error}>{{ error }}</span>
</p>
```

```
<Field name="email" label="Email" value={form.email} error={errors.email} />
```

One component instead of three lines of markup per form field. A working
example with a `<textarea>` variant is `examples/cookbook/components/Field.rhx`.

**Toasts.** `hx.toast(message, type)` (7.5) on the server sends a `showToast`
event via the `HX-Trigger` header; `ui.js` draws it into `#toasts` (creating
the container itself if the layout has none). Types are `info` (default),
`success`, `error`, `warning`. A click dismisses a toast; otherwise it goes
after `window.__rhaix.toastTimeout` ms (4000; `0` keeps it). Several
`hx.toast(...)` calls in one request all arrive: the detail is the first
toast, plus an `items` array with all of them.

The default styles have zero specificity (`:where(...)`), so any rule of
yours — even a plain `.toast { … }` — wins.

**A toast before a redirect is not lost (flash).** `hx.toast("Welcome");
res.redirect("/admin")` is the most common pairing. On its own nobody would
ever see that toast: htmx fires the `HX-Trigger` event and immediately leaves
for the new address, and a plain form without htmx gets a `303` with no toast
at all. So the toasts of a response that goes elsewhere (`res.redirect`,
`hx.redirect`, `hx.refresh`) are stored in the session, and the very next
non-redirect response delivers them — once. An htmx request gets them as a
`showToast` event, a full page as an embedded
`<script type="application/json" data-rhx-toasts>` that `ui.js` picks up. The
toasts of a plain (non-htmx) page load travel the same way, since the browser
does not read `HX-Trigger` there. Nothing to do on your side — it only needs a
session (there is one in `pages/`, none in `api/`).

**`<dialog>` opens itself as a real modal.** Write `<dialog>` instead of
`<div class="modal">` — `ui.js` calls `showModal()` for every `<dialog>`,
wherever it shows up: on the first load, in a fragment, outside the main
target via `@oob`. Native backdrop, `Esc`, focus trap — with no code in the
project at all. It closes itself the moment the element is removed from the
page (an empty response to the same `hx-target` is the usual way to close a
dialog). A project that genuinely wants a plain, non-modal `<dialog>` just
adds `data-plain`. A working example is
`examples/cookbook/partials/OrderCard.rhx`.

**Changing the interface — two levels.**

Piece by piece: override one function in `public/*.js`. It is read at the
moment of the event, so load order does not matter:

```js
// public/app.js
window.__rhaix.toast = (message, type) => myToastLib.show(message, type);
window.__rhaix.toastTimeout = 8000;
```

Wholesale: `rhaix eject ui` copies the built-in `ui.js` into
`public/rhaix-ui.js`. As soon as that file exists, the framework loads it
**instead of** `/_rhaix/ui.js` — from then on it is ordinary project code:
edit it, rewrite it on top of your own library, or leave it empty to turn off
both toasts and auto-modals entirely. Any replacement has to honour the same
contract: listen for `showToast` (with `items` for several toasts), show the
toasts from `script[data-rhx-toasts]` in the page and, if you like, do
something with your `<dialog>` elements.

```
rhaix eject ui            # in the current project
rhaix eject ui --force    # overwrite an already ejected file with the built-in one
```

### 7.8 Live updates — `live.send`

A page updates itself as soon as someone else changes the data:

```rhai
// where the data changes — a page, api/, anything
db.insert("orders", order);
live.send("orders");                 // or live.send("orders", #{ id: id })
```

```html
<!-- where it is shown -->
<div hx-get="/orders" hx-trigger="live:orders from:body"
     hx-select="#orders" hx-target="this" hx-swap="outerHTML" id="orders">…</div>
```

The server sends only a signal, "this topic changed", not HTML. Each page
re-requests its own data with its own session and permissions. So nothing a
given user should not see can leak through the channel, and the server does not
render a page per subscriber.

The `rhaix.js` core collects the topics mentioned on the page
(`hx-trigger="live:…"`, or `data-live="orders users"` for your own JS) and keeps
**one** SSE subscription to `/_rhaix/live`. The event arrives on `<body>` as
`live:<topic>` with the server's detail. After a dropped connection every topic
gets an event with `{ reconnected: true }`, so whatever was missed is fetched
again.

A topic is Latin letters, digits, `_`, `-`, `.`, up to 64 characters.
`live.send` refuses anything else: the topic becomes a DOM event name and part
of a URL. The channel lives in process memory: two copies behind a load
balancer each notify their own visitors. For infrequent changes (orders,
statuses, a small team's chat) that is enough. Behind a proxy such as nginx SSE
works out of the box: the response carries `X-Accel-Buffering: no`. Recipe:
`examples/cookbook/pages/live.rhx`.

### 7.9 Your own Rust functions — `native/`

When Rhai is not enough (heavy computation, a Rust crate, a fast parser), the
project writes a plain Rust function:

```rust
// native/lib.rs
use rhaix_server::rhai::Engine;

pub fn register(engine: &mut Engine) {
    engine.register_fn("fib", |n: i64| -> i64 {
        let (mut a, mut b) = (0_i64, 1_i64);
        for _ in 0..n.clamp(0, 90) { (a, b) = (b, a + b); }
        a
    });
}
```

```html
<p>{{ fib(50) }}</p>
```

- **`rhaix build`** compiles `native/lib.rs` into the binary along with
  everything else.
- **`rhaix dev` and `rhaix serve`**, seeing `native/`, build their own server
  with `cargo` (in `target/rhaix-native/`) and run it. Templates still reload
  live as usual. After editing `native/`, just restart `rhaix dev`: cargo
  rebuilds only what changed. This needs a Rust toolchain.
- **Dependencies of your code** go into `native/dependencies.toml` as Cargo
  lines (`regex = "1"`) and are added to the generated manifest.
- **`rhaix_server::rhai`** is exactly the Rhai the scripts run on. Your own
  `rhai` dependency in another version would be a different `Engine` type, and
  `register` would not compile.
- **Your own binary instead of the CLI:** if you embed rhaix in your own
  program, use `Config::load(".", None)?.with_native(my::register)`.

A function in `scripts/*.rhai` with the same name overrides the Rust one, so do
not reuse names.

---

## 8. `<style>` and `<script>` in a component

```html
<div class="card">…</div>

<style>
  /* in v1 — global CSS, hoisted into <rhaix:head/> and deduplicated by hash */
  .card { border: 1px solid #ddd }
</style>

<script>
  // runs once per page lifetime, even if the component
  // arrived through several htmx swaps
  const data = {{ json(props) }};
  console.log("card ready", data);
</script>
```

- `<style>` is **global**: hoisted into `<rhaix:head/>`, deduplicated by content
  hash.
- `<style scoped>` **narrows selectors to its own file's markup**. The core
  derives a stable id from the file path, stamps `data-rhx-<hash8>` on every
  element of that file and appends `[data-rhx-…]` to the **last** compound
  selector (`.card .title` → `.card .title[data-rhx-…]`). Slot content stays in
  the parent's scope; `@media`/`@supports` are entered, `@keyframes` and
  `@font-face` are left alone; `:root` will not match inside a scoped style —
  keep theme variables in a plain `public/*.css`.
- `<script>` is hoisted into `<rhaix:scripts/>` and runs **once per page
  lifetime**. In a fragment it travels with the markup, wrapped in a registry
  check, so a repeated swap does not run it again. The registry and htmx
  already exist at that point (they are in `<head>`), but `public/**.js` do
  not yet: they have `defer` and run after the page is parsed. Reach for them
  from event handlers, not at the top level of a component script.
- Tags with `src` are not hoisted: they load once anyway, and their position in
  the document often matters.
- There is no hoisting in a layout — the layout is the document, its tags stay
  where they are.
- Inside `<script>` only `{{ json(x) }}` works (see 2.5); inside `<style>`
  interpolation is forbidden.
- **rhaix has no islands (`<script client>`).** Client-side interactivity means
  HTMX attributes, ordinary `public/*.js` files and hoisted component
  `<script>`.

---

## 9. Errors

Categories and where they arise:

| Stage | Example |
|---|---|
| Parsing | unclosed tag, `@else` without `@if`, `{{` without `}}` |
| Resolution | unknown component, circular dependency, unknown slot |
| Expression compilation | a Rhai syntax error inside `{{ }}` or `{ }` |
| Execution | `undefined variable`, `throw`, exceeding a limit |

Format (in dev — in the console and as an overlay in the browser; in production —
in the log):

```
error: unknown variable `todoz`
  ┌─ pages/todo.rhx:7:26
  │
7 │   <TodoItem @for={t in todoz} todo={t} />
  │                        ^^^^^ did you mean `todos`?
  │
  = chain: pages/todo.rhx → components/TodoList.rhx
```

**`rhaix check` warnings.** Besides errors, `check` names what compiles but
does the wrong thing. Warnings do not change the exit code.

| What | Why it is a trap |
|---|---|
| `<td>{c.name}</td>` | in text single braces are just characters (2.4): the literal `{c.name}` is printed; use `{{ c.name }}` |
| `@class={#{"selected": …}}` | `selected`, `checked`, `disabled` and the like are attributes; in `@class` they become a class; use `@attr` |
| a layout without `<rhaix:head/>` | framework scripts rerun on boosted navigation, and without `<rhaix:scripts/>` htmx is not loaded at all |
| an unknown key in `rhaix.toml` | the framework does not read `minify_html = true` or `sesion_days = 7`; the nearest known key is suggested |

Unknown config keys are also logged when the server starts.


---

## 10. Reserved

- The `rhaix:` tag prefix — core only.
- Attributes starting with `@` — directives only.
- `data-rhx-*` attributes — internal.
- Component names `Fragment`, `Slot` — internal.
- **[v1.1]**: scoped slots, `@key` for morph swaps, `@transition`.
- **Not planned**: islands (`<script client>`), partial hydration, client-side
  components — deliberately outside the framework.

---

## 11. Grammar (simplified EBNF)

See [SYNTAX.md](SYNTAX.md) § 11 — the grammar is language-independent and is not
duplicated here.

## 12. A complete example

See [examples/cookbook](examples/cookbook): every file there answers one task,
and a test in `crates/rhaix-server/tests/examples.rs` fails the build if a recipe
stops compiling.
