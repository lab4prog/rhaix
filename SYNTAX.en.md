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

### 4.8 Summary table

| Directive | On what | Value |
|---|---|---|
| `@if` `@else-if` `@else` | any element, `<template>`, component | expression / — |
| `@for` | the same | `x in coll`, `(x, i) in coll` |
| `@key` | together with `@for` | expression |
| `@class` `@style` `@attr` | HTML elements | map / array / string |
| `@html` `@text` | HTML elements | expression |
| `@oob` | HTML elements, components | selector expression (4.7) |

Directives on a component: `@if/@else*/@for/@key` are allowed. `@class`/`@attr`
on a component is an error — a component decides its own markup, so pass props.

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
| `<rhaix:head/>` | `public/**.css` plus hoisted component `<style>` |
| `<rhaix:scripts/>` | `public/**.js`, `rhaix.js`, hoisted component `<script>` |
| `<rhaix:csrf/>` | a hidden CSRF field, when the automatic one cannot be placed (7.2) |
| `<rhaix:raw>…</rhaix:raw>` | a block emitted without processing |

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

- A request **without** `HX-Request` → page plus layout.
- A request **with** `HX-Request` → only the page's or partial's own markup, no
  layout.

This is automatic — there is nothing to switch on. Every response carries
`Vary: HX-Request`.

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
| `state` | everywhere | process-wide store: `state.get/set/has/remove` |
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
- `Set-Cookie` appears only when the session was actually modified. A page that
  writes nothing carries no cookie at all.

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

```rhai
slug("Привіт, світе!")     // "pryvit-svite" — transliteration per Ukrainian standard
cut(text, 20)              // trim at a word boundary and add "…"
strip_tags(text)
capitalize(text)
money(1234.5)              // "1 234,50" (non-breaking space, rounds away from zero)
money(value, 0)
uuid() / random_id() / sha256(text)
json_encode(value) / json_decode(text)
is_blank(value)            // (), "", "   ", [], #{}
```

`json()` and `json_encode()` are different things: the first escapes whatever
could close a tag and is meant for `<script>` (2.5); the second gives a plain
string for a database or an API.

Password hashing is deliberately not part of v1: a correct `hash_password` is
Argon2 with tuned parameters, and passing SHA-256 off as one would be dishonest.
It is part of M12, together with the rest of authentication.

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

- `<style>` in v1 is **global**: hoisted into `<rhaix:head/>`, deduplicated by
  content hash. Scoping (`data-rhx-<hash8>` plus selector rewriting) needs a full
  CSS parser and is deferred to **[v1.1]**; the `<style scoped>` attribute is
  reserved for it. For now isolation is a matter of class-naming convention.
- `<script>` is hoisted into `<rhaix:scripts/>` and runs **once per page
  lifetime**. In a fragment it travels with the markup, wrapped in a registry
  check from `rhaix.js`, so a repeated swap does not run it again.
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

---

## 10. Reserved

- The `rhaix:` tag prefix — core only.
- Attributes starting with `@` — directives only.
- `data-rhx-*` attributes — internal.
- Component names `Fragment`, `Slot` — internal.
- **[v1.1]**: scoped CSS (`<style scoped>`), scoped slots, `@key` for morph
  swaps, `@transition`, i18n tags.
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
