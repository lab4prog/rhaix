# rhaix — специфікація синтаксису `.rhx` (v1)

Усе, що позначено **[v1.1]**, свідомо винесено за межі першої версії.
English version: [SYNTAX.en.md](SYNTAX.en.md). Для LLM: [llms.txt](llms.txt).
Готові рецепти «задача → файл»: [examples/cookbook](examples/cookbook).

---

## 0. Шпаргалка на один екран

```
---                                   // frontmatter: Rhai, у вивід не потрапляє
let todos = db.query("select * from todos");
let title = "ToDo";
page.title = title;
---
<h1>{{ title }}</h1>                       {{ вираз }}        — вивід з екрануванням
<div>{{ raw(post.body) }}</div>            raw(...)           — без екранування
{{! це коментар, його не видно у HTML }}

<ul>
  <TodoItem @for={t in todos} @key={t.id} todo={t} />        цикл
  <li @if={todos.is_empty()}>Порожньо</li>                   умова
  <li @else>Всього: {{ todos.len() }}</li>                   інакше
</ul>

<a href="/todo/{{ t.id }}"                 інтерполяція в атрибуті
   class="btn" @class={#{"active": is_on}} @attr={#{"disabled": locked}}>
  {{ t.title }}
</a>

<Card>                                     компонент зі слотом
  <template slot="header"><h2>Заголовок</h2></template>
  Вміст
</Card>
```

Правила, які не треба запам'ятовувати — вони єдині:

1. Тег з **великої** літери → компонент із `components/`.
2. Атрибут з `@` → директива фреймворку, у HTML не виводиться.
3. `{ ... }` у значенні атрибута → Rhai-вираз; `"..."` → рядок (з `{{ }}` усередині).
4. Усе інше — звичайний HTML, який віддається як є.

---

## 1. Файл `.rhx`

```
file        = [ frontmatter ] , template ;
frontmatter = "---" , NEWLINE , rhai-code , NEWLINE , "---" , NEWLINE ;
```

- Відкривальний `---` має бути **першим** непорожнім рядком файлу.
- Вміст між `---` — код мовою [Rhai](https://rhai.rs), виконується на сервері
  перед рендером розмітки. У відповідь не потрапляє **нічого** з цього блоку.
- Без frontmatter файл є чистим HTML-шаблоном (валідно й нормально).
- Кодування — UTF-8. Перенос рядків — LF або CRLF (нормалізується).

### 1.1 Види файлів

| Тека | Роль | Доступ по HTTP |
|---|---|---|
| `pages/**/*.rhx` | сторінка / ендпоінт | так, за шляхом файлу |
| `partials/**/*.rhx` | фрагмент для HTMX | так, `/components/<name>` |
| `components/**/*.rhx` | компонент | **ні** |
| `layouts/*.rhx` | обгортка повної сторінки | ні |
| `scripts/*.rhai` | спільні Rhai-функції, доступні скрізь без підключення | ні |
| `middleware.rhx` | код, що виконується перед кожним запитом (6.6) | ні |

---

## 2. Текст, вивід і екранування

### 2.1 Інтерполяція

```
interp = "{{" , [ "-" ] , rhai-expr , [ "-" ] , "}}" ;
```

```html
<p>{{ user.name }}</p>
<p>{{ price * qty }} грн</p>
<p>{{ if done { "✔" } else { "…" } }}</p>
<p>{{ todos.filter(|t| !t.done).len() }}</p>
```

- Усередині — **будь-який вираз Rhai** (не інструкція: без `let`, `;`, `while`).
- Вираз компілюється один раз при завантаженні файлу і кешується як `rhai::AST`.
- Результат приводиться до рядка: `()` та `false` дають **порожній рядок**
  (щоб `{{ maybe_null }}` не друкував `()`), решта — через `to_string()`.
- Екранується завжди: `&` `<` `>` `"` `'` → HTML-entity. У атрибутах, `<script>`
  і `<style>` правила інші — див. 2.5.

**Обрізання пробілів:** `{{-` прибирає пробіли/переноси зліва, `-}}` — справа.

```html
<td>
  {{- total -}}
</td>
```
→ `<td>1500</td>`

### 2.2 Сирий вивід

```html
{{ raw(article.html) }}
```

`raw(s)` повертає значення типу `Html`, яке не екранується. Це єдиний спосіб
вставити готовий HTML. Значення типу `Html` (результат `raw()`, рендер
компонента) не екрануються повторно.

### 2.3 Коментарі

| Запис | Поведінка |
|---|---|
| `{{! текст }}` | rhaix-коментар, у вивід **не** потрапляє |
| `<!-- текст -->` | звичайний HTML-коментар, віддається клієнту (у `--release` вирізається, якщо `minify_html = true`) |

### 2.4 Літеральні дужки

```html
<p>{{ "{{" }} не інтерполяція {{ "}}" }}</p>
```
Або блоком:
```html
<rhaix:raw>
  усе всередині віддається дослівно: {{ це не вираз }}
</rhaix:raw>
```

---

### 2.5 Контекст виводу (безпека)

Екранування залежить від того, **де** стоїть `{{ }}`. Це не налаштування — ядро
визначає контекст при компіляції і застосовує відповідне правило.

| Контекст | Правило |
|---|---|
| текст у розмітці | HTML-екранування `& < > " '` |
| значення атрибута | атрибутне екранування + правила нижче |
| `href` `src` `action` `formaction` `xlink:href` `poster` `data` | додатково перевірка схеми URL |
| `on*` (`onclick`, `onerror`, …) | **вираз заборонено**; лише статичний рядок, написаний автором |
| `<script>` | лише `{{ json(x) }}`; голий вираз — помилка компіляції |
| `<style>` | інтерполяція **заборонена** |
| `{{ raw(x) }}` | без екранування — відповідальність автора |

**Схеми URL.** Дозволені: відносні шляхи, `http`, `https`, `mailto`, `tel`, `ftp`,
`data:image/*`. Усе інше (насамперед `javascript:`, `vbscript:`, `data:text/html`)
замінюється на `#` із попередженням у лозі. Це рятує від класичного
`<a href={row.link}>`, де `link` прийшов з БД від користувача.

**Обробники подій.** `onclick={expr}` — помилка компіляції. Ключі `on*`, що
приходять у `@attr` або `{...spread}`, відкидаються під час рендеру з
попередженням у лозі: мапа збирається в рантаймі, тож перевірити її наперед
неможливо. Статичний `onclick="alert(1)"`, написаний прямо в розмітці, працює
як завжди.

Обробником вважається **будь-який** атрибут, що починається на `on`: стандартного
HTML-атрибута з такою назвою, який не є обробником, не існує, а помилитись тут
дорожче, ніж перестрахуватись.

**Дані для JS.** Єдиний спосіб передати значення в скрипт:

```html
<script>
  const todo = {{ json(todo) }};       // JSON + екранування `<`, U+2028/2029
  const ids  = {{ json(ids) }};
</script>
```

`{{ todo.title }}` усередині `<script>` — помилка компіляції з підказкою
«використайте `json()`». Так закривається XSS, який HTML-екранування не ловить,
бо всередині JS-рядка воно не діє.

---

## 3. Атрибути

```
attribute = directive | interpolated | expression | boolean | spread ;

interpolated = name , "=" , '"' , { text | interp } , '"' ;
expression   = name , "=" , "{" , rhai-expr , "}" ;
boolean      = name ;
spread       = "{" , "..." , rhai-expr , "}" ;
directive    = "@" , name , [ "=" , "{" , rhai-expr , "}" ] ;
```

```html
<a href="/todo/{{ t.id }}?tab={{ tab }}">…</a>   <!-- інтерполяція в рядку -->
<a href={link}>…</a>                              <!-- вираз -->
<input required>                                  <!-- булевий -->
<input {...field_attrs}>                          <!-- розпакування мапи -->
```

**Значення-вираз → атрибут:**

| Результат виразу | HTML |
|---|---|
| `"abc"`, `42` | `attr="abc"` / `attr="42"` |
| `true` | `attr` (без значення) |
| `false` або `()` | атрибут **не виводиться** |
| масив | значення через пробіл |
| мапа | `key="value"` пари (для `@attr`) |

Значення завжди екранується для контексту атрибута. Атрибути в лапках можуть
містити `{{ }}`; поза лапками — тільки `{ }`.

Для `href`/`src`/`action`/`formaction` діє перевірка схеми URL, а `on*`-атрибути
не можуть бути результатом виразу або `{...spread}` — див. 2.5.

---

## 4. Директиви

Директиви обробляються ядром і ніколи не потрапляють у вивід.

### 4.1 `@if` / `@else-if` / `@else`

```html
<p @if={user.is_admin}>Адмін</p>
<p @else-if={user.is_editor}>Редактор</p>
<p @else>Гість</p>
```

- `@else-if` / `@else` мають бути **сусіднім елементом** після `@if`
  (між ними дозволені лише пробіли та коментарі). Інакше — помилка компіляції.
- Умова — Rhai-вираз; truthiness: `false`, `()`, `0`, `""`, порожній масив/мапа
  вважаються хибними (свідомо м'якше за Rhai, бо шаблон працює з даними з БД).
- Щоб сховати групу елементів без обгортки — `<template @if={...}>`.

### 4.2 `@for`

```html
<li @for={todo in todos}>{{ todo.title }}</li>
<li @for={(todo, i) in todos}>{{ i + 1 }}. {{ todo.title }}</li>
<option @for={(v, k) in options} value={k}>{{ v }}</option>   <!-- по мапі -->
<span @for={n in 1..=5}>{{ n }}</span>                        <!-- по діапазону -->
```

Усередині доступний об'єкт `iter`:

| Поле | Значення |
|---|---|
| `iter.index` | 0-based індекс |
| `iter.number` | 1-based |
| `iter.first` / `iter.last` | `bool` |
| `iter.count` | довжина колекції |

Чому `iter`, а не `loop`: `loop` — ключове слово Rhai, і вираз `{{ loop.number }}`
просто не компілюється. Виявлено при реалізації M1.

Мапа лічильників будується лише тоді, коли `iter` справді згадується в тілі
циклу — на таблиці в 1000 рядків це помітно.

`@key={expr}` — необов'язковий. У v1 не впливає на вивід; зарезервовано для
morph-свопів (idiomorph) і буде виводитись як `data-rhx-key`, коли їх увімкнено.

**Порядок із `@if`:** `@for` зовнішній, `@if` перевіряється **на кожній ітерації**
(тому в умові можна використовувати змінну циклу):

```html
<li @for={t in todos} @if={!t.done}>{{ t.title }}</li>
```

Щоб перевірити умову **до** циклу — обгорнути:

```html
<template @if={!todos.is_empty()}>
  <li @for={t in todos}>…</li>
</template>
```

### 4.3 `@class`

Мерджиться зі статичним `class`, не замінює його.

```html
<li class="todo-item" @class={#{"completed": t.done, "urgent": t.priority > 5}}>
<li @class={["a", "b"]}>
<li @class={some_string}>
```

### 4.4 `@style`

```html
<div @style={#{"width": pct + "%", "color": c}}>
```
`()`/`false` як значення — властивість пропускається.

### 4.5 `@attr`

Динамічний набір атрибутів однією мапою.

```html
<input @attr={#{"checked": t.done, "disabled": locked, "value": t.title}}>
```

Ключі `on*` у мапі — помилка компіляції (якщо літерал) або відкидаються в
рантаймі з попередженням (якщо мапа зібрана динамічно). Див. 2.5.

### 4.6 `@html` / `@text`

Замінюють вміст елемента (діти в розмітці при цьому мають бути відсутні).

```html
<div @html={post.body_html}></div>        <!-- сирий HTML -->
<div @text={user_input}></div>            <!-- екранований текст -->
```

### 4.7 `@oob` — фрагмент поза основною ціллю

HTMX уміє замінювати не лише те, що просив клієнт: елемент із `hx-swap-oob`
підміняє свій відповідник на сторінці. Це щоденний сценарій внутрішніх
інструментів — зберегли в модалці, а оновити треба рядок таблиці позаду.

```html
<tr @oob={"#row-" + order.id}>
  <td>{{ order.id }}</td><td>{{ order.title }}</td>
</tr>
```

→ `<tr id="row-42" hx-swap-oob="outerHTML:#row-42">…</tr>`

Ядро саме проставляє `hx-swap-oob` і, якщо селектор має вигляд `#id`, ще й `id`.
Спосіб заміни задається другим значенням: `@oob={["#list", "beforeend"]}`.
Без директиви довелось би повертати HTML рядком із логіки — перевірено в
`examples/ergonomics/order-modal.rhx`, виглядає погано.

### 4.8 Зведена таблиця

| Директива | На чому | Значення |
|---|---|---|
| `@if` `@else-if` `@else` | будь-який елемент, `<template>`, компонент | вираз / — |
| `@for` | те саме | `x in coll`, `(x, i) in coll` |
| `@key` | разом із `@for` | вираз |
| `@class` `@style` `@attr` | HTML-елементи | мапа / масив / рядок |
| `@html` `@text` | HTML-елементи | вираз |
| `@oob` | HTML-елементи, компоненти | вираз-селектор (4.7) |

Директиви на компоненті: дозволені `@if/@else*/@for/@key`. `@class`/`@attr` на
компоненті — помилка (компонент сам вирішує свою розмітку; передавайте props).

---

## 5. Компоненти

### 5.1 Іменування та резолв

```html
<TodoItem />              → components/TodoItem.rhx
<Ui.Button />             → components/ui/Button.rhx
<Forms.Field.Text />      → components/forms/field/Text.rhx
```

- Тег вважається компонентом, якщо починається з великої латинської літери.
- Крапка = роздільник тек; сегмент після останньої крапки — ім'я файлу
  (регістр як у тезі), сегменти тек — у нижньому регістрі.
- Якщо файл не знайдено — помилка компіляції з підказкою найближчого імені:
  `компонент "TodoItm" не знайдено; можливо, "TodoItem"?`
- Циклічні залежності (`A → B → A`) виявляються при компіляції, а не в рантаймі.

### 5.2 Props

```html
<TodoItem todo={t} editable compact="yes" {...rest} />
```

Усередині компонента:

```
---
let todo    = props.todo;                  // обов'язковий; відсутній → () 
let editable= props.editable ?? false;     // значення за замовчуванням
let compact = props.compact ?? "no";
---
```

- `props` — мапа з усіма переданими значеннями.
- Для зручності кожен prop також доступний як **змінна з тим самим іменем**
  (тобто `todo` працює і без `let todo = props.todo`), але явне оголошення
  рекомендоване — воно самодокументує компонент.
- Імена з дефісами доступні тільки через `props["data-x"]`.
- Валідація — звичайним кодом:

```
---
if props.todo == () { throw "TodoItem: пропущено обов'язковий prop `todo`"; }
---
```

`throw` у компоненті дає помилку з ланцюжком: `pages/todo.rhx:12 → TodoList.rhx:4 → TodoItem.rhx:2`.

### 5.3 Ізоляція області видимості

Компонент **не бачить** змінних батька — лише `props`, глобальні об'єкти
(`req`, `db`, `page`, …) та власний frontmatter. Це навмисно: так компонент
залишається переносимим, а помилки — локальними.

### 5.4 Слоти

```html
<!-- components/Card.rhx -->
<div class="card">
  <header @if={slots.has("header")}><slot name="header"/></header>
  <div class="card-body"><slot>Порожньо</slot></div>
  <footer><slot name="footer"/></footer>
</div>
```

```html
<Card>
  <template slot="header"><h2>{{ title }}</h2></template>
  <p>Основний вміст</p>
  <button slot="footer">OK</button>
</Card>
```

- `<slot/>` без імені — вміст за замовчуванням; `<slot name="x"/>` — іменований.
- Вміст усередині `<slot>…</slot>` — фолбек, якщо слот не передано.
- Слот віддається в **області видимості батька** (бачить змінні батька, не компонента).
- `slots.has("name")` → `bool`.
- Scoped-слоти (передача даних зі слота назад) — **[v1.1]**.
- Слот вважається переданим, якщо його вміст не порожній: `<Card></Card>` покаже
  запасний вміст, а не порожнє місце.

### 5.5 Групувальні теги

| Тег | Роль |
|---|---|
| `<template>` | носій директив, сам не виводиться |

`<template @if={...}>` або `<template @for={...}>` — єдиний спосіб повісити
директиву на групу елементів без зайвої обгортки в HTML. Окремого `<Fragment>`
немає: другий запис для тієї самої речі лише додає, що запам'ятовувати.

---

## 6. Layout і сторінки

### 6.1 Layout

`layouts/main.rhx` — єдине місце з повним `<html>`. Сторінка потрапляє в `<slot/>`.

```html
---
let nav = [#{href:"/", t:"Home"}, #{href:"/todo", t:"ToDo"}];
---
<!DOCTYPE html>
<html lang="uk">
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

| Спецтег | Що вставляє |
|---|---|
| `<rhaix:head/>` | зібрані `<style>`, `<link>`, `<meta>` компонентів + `page.head` |
| `<rhaix:scripts/>` | `htmx.min.js`, `rhaix.js`, `public/**.js`, hoisted `<script>` |
| `<rhaix:raw>…</rhaix:raw>` | блок без обробки |

### 6.2 Вибір layout

```
---
page.layout = "admin";     // layouts/admin.rhx
page.layout = false;       // без layout навіть при повному завантаженні
---
```

За замовчуванням — `layouts/main.rhx`, якщо він існує. Обгортка задається через
ту саму мапу `page`, що й `page.title`: окремої функції `layout()` немає
навмисно — один спосіб замість двох.

В імені дозволені літери, цифри, `-` і `_`. Усе інше ігнорується з
попередженням: значення може прийти з форми, а `page.layout = "../../etc/passwd"`
не має нікуди вести.

### 6.3 Правило фрагмента

- Запит **без** `HX-Request` → сторінка + layout.
- Запит **з** `HX-Request` → лише розмітка сторінки/партіала, без layout.

Це автоматично, керувати не треба. Відповідь завжди має `Vary: HX-Request`.

### 6.4 Маршрути

| Файл | Маршрут | Доступ до параметра |
|---|---|---|
| `pages/index.rhx` | `/` | — |
| `pages/todo.rhx` | `/todo` | — |
| `pages/todo/[id].rhx` | `/todo/:id` | `req.param("id")` |
| `pages/blog/[...slug].rhx` | `/blog/*` | `req.param("slug")` |
| `partials/Stats.rhx` | `/components/stats` | — |

Маршрути задає **тільки структура тек**. Оголошення `route()` і `methods()`
у frontmatter, які були в чернетці спеки, прибрані: щоб їх прочитати, довелося б
виконувати логіку сторінки під час старту — без запиту, без `req`, наосліп.
Замість цього:

- нестандартний шлях — це просто інша назва файлу або тека;
- метод розбирається в самій логіці (`if req.method == "POST"`), і це видно
  в тому ж файлі, а не в окремому оголошенні.

Файли в `partials/` віддаються **завжди як фрагмент**: layout до них не
додається навіть при прямому заході.

### 6.5 Логіка за методом і ранній вихід

```
---
if req.method == "POST" {
  db.exec("insert into todos(title) values(?)", [req.form("title")]);
  hx.toast("Додано", "success");
  hx.trigger("todoChanged");
}
if req.method == "DELETE" {
  db.exec("delete from todos where id=?", [req.query("id")]);
  return "";                     // порожня відповідь — htmx видалить елемент
}
if session.user == () {
  res.redirect("/login");
  return;                        // рендер розмітки не виконується
}
let todos = db.query("select * from todos order by id");
---
<TodoList todos={todos} />
```

- `return;` — віддати відповідь без рендеру розмітки (для redirect/204).
- `return <рядок>` — віддати саме цей рядок (екранування не застосовується;
  для HTML використовуйте `raw()` або просто рядок — вміст вважається готовим).
- Рендер розмітки скасовують лише `res.redirect`, `hx.redirect` і `hx.refresh`.
  `res.status(422)` статус міняє, але сторінку все одно рендерить — саме це
  потрібно формі з помилками валідації.
- Будь-яке значення, яке лишає по собі frontmatter, стає тілом відповіді. Тому
  останній рядок завершуйте `;` — інакше результат виразу поїде клієнтові
  замість розмітки.

### 6.6 `middleware.rhx`

Файл у корені проєкту, який виконується **перед** сторінкою — для того, що
інакше довелось би дублювати в кожному файлі: автентифікація, права, локаль,
логування.

```
middleware.rhx
---
state.started = now();

if req.path.starts_with("/admin") {
    if session.user == () { hx.redirect("/login"); return; }
    if session.user.role != "admin" { res.status(403); return "Немає доступу"; }
}
---
```

- Розмітки у файлі немає — лише frontmatter (закривальний `---` усе одно потрібен).
- `state`, `req`, `res`, `hx`, `log` доступні так само, як на сторінці.
- `return;` зупиняє обробку: сторінка не виконується взагалі.
- Порядок: `middleware.rhx` → сторінка → layout.

Без цього охорона доступу переписується в кожну закриту сторінку, і забутий
рядок в одному файлі відкриває сторінку всім (перевірено в
`examples/ergonomics/admin-report.rhx`).

---

## 7. Область видимості та глобальні об'єкти

| Ім'я | Де доступне | Що це |
|---|---|---|
| змінні frontmatter | цей файл | `let x = …` |
| `props` | компонент/партіал | передані значення |
| `slots` | компонент | `slots.has(name)` |
| `iter` | усередині `@for` | лічильники ітерації |
| `page` | усюди | спільна мапа сторінки: `page.title`, `page.head`, `page.class` |
| `req` `res` `hx` | усюди | запит/відповідь/HTMX |
| `state` | усюди | процесне сховище: `state.get/set/has/remove` |
| `db` | усюди | база: `query/one/exec` і `find/get/count/insert/update/delete` |
| `session` | усюди | підписаний cookie: `session.get/set/has/remove/clear/all`, `session.user` (7.2) |
| `csrf` | усюди | `csrf.token` — токен поточної сесії (7.2) |
| `http` | усюди | виклик чужого API: `http.get/post/put/patch/delete` (7.3) |
| `env` `log` | усюди | оточення й лог |
| хелпери | усюди | `url()`, `now()`, `date()`, `slug()`, `money()`, `cut()`, `uuid()`, `json()` (для `<script>`), `json_encode/decode()`, `percent()`, `raw()` … (7.4) |

**`url(path, params)`** — єдиний правильний спосіб зібрати посилання зі станом:

```html
<a href={url("/orders", #{ q: q, sort: sort, page: n })}>{{ n }}</a>
```

Параметри кодуються, `()` і порожні рядки пропускаються. Ручна конкатенація
(`"/orders?q=" + q`) ламається на `&` у значенні — в доках її немає навмисно.

**Типізований доступ до запиту.** `req.query()` і `req.form()` повертають рядок,
тому для чисел і прапорців є `req.query_int/query_float/query_bool` і
`req.form_int/form_float/form_bool`. Кожен повертає `()`, якщо значення немає
або воно не парситься — далі звичайне `?? 1`.

**Спільні функції — `scripts/*.rhai`.** Підключати нічого не треба: усе, що
там оголошено, видно з будь-якого `.rhx` — і у frontmatter, і в `{{ }}`.

```rhai
// scripts/orders.rhai
fn status_label(status) {
    switch status { "new" => "Новий", "paid" => "Оплачено", _ => status }
}
```

```html
<!-- pages/table.rhx — жодного import -->
<span>{{ status_label(o.status) }}</span>
```

Файли підключаються **один раз при старті**, тому правка `scripts/*.rhai`
вимагає перезапуску `rhaix dev` — він про це скаже в консолі. Зламаний спільний
скрипт зупиняє старт, а не перший запит.

`import "helpers" as h;` теж працює (модулі беруться з тієї самої теки й не
можуть з неї вийти), але потрібен рідко: підключений модуль живе лише в межах
одного блоку коду, тому з frontmatter він **не** доживає до `{{ }}`.

---

### 7.1 Дві особливості Rhai, про які треба знати

1. **`loop` — ключове слово**, тому лічильники циклу живуть в `iter` (4.2).
2. **Багато методів рядків працюють на місці.** `trim()` у самому Rhai підрізає
   рядок і повертає `()`, через що `let title = s.trim();` давав порожнє
   значення. У rhaix `trim()` перевизначено: він так само підрізає на місці, але
   ще й повертає результат, тож обидва записи роблять очікуване.
3. **У SQLite немає булевого типу.** `done` приїжджає як `0`/`1`. У шаблоні це
   не заважає (`@if={t.done}` користується істинністю rhaix), `!t.done` теж
   працює — заперечення перевизначене за тим самим правилом. А от `if t.done`
   у frontmatter — це вже чистий Rhai, який вимагає саме `bool`, тому пишеться
   `if bool(t.done)`.

---

### 7.2 Сесія і CSRF

```rhai
session.set("user", name);      // покласти
session.user                    // дістати `user` (скорочення від session.get("user"))
session.get("cart") ?? []       // усе інше
session.has("user")
session.remove("cart")
session.clear();                // вихід
```

Сесія — це **підписаний cookie**, а не запис на сервері. Наслідки, які треба
знати одразу:

- Стан їде до клієнта й назад, тому застосунок можна запустити у двох копіях за
  балансиром — і жодної спільної пам'яті не треба.
- Клієнт **бачить** вміст (base64 — не шифр), але не може його змінити: підпис
  HMAC-SHA256 не зійдеться, і сесія просто стане порожньою. Паролі й будь-що
  таємне туди не кладуть.
- Розмір обмежений ~4 КБ. У сесії тримають ідентифікатор, у базі — дані.
- `session.clear()` гасить cookie **у цього браузера**. Відкликати вже виданий
  cookie на сервері не можна — його можна лише дочекатись до кінця терміну або
  змінити секрет.
- `Set-Cookie` з'являється тільки тоді, коли сесію справді змінювали. Сторінка,
  яка нічого не пише, не тягне за собою жодного cookie.

**CSRF працює сам.** Форму, що змінює дані, фреймворк доповнює прихованим полем:

```html
<form method="post" action="/login">   <!-- або hx-post="/login" -->
  <input name="name">
</form>
```

```html
<!-- у відповіді -->
<form method="post" action="/login"><input type="hidden" name="_csrf" value="…">
  <input name="name">
</form>
```

Елементу, який змінює дані **без форми**, дописується заголовок:

```html
<button hx-delete={url("/todo", #{ id: t.id })}>×</button>
<!-- → hx-headers='{"x-csrf-token": "…"}' -->
```

Запит із методом, відмінним від `GET`/`HEAD`/`OPTIONS`, без дійсного токена не
доходить навіть до `middleware.rhx` — у відповідь іде `403` із текстом
«Термін дії форми минув».

| Випадок | Що робити |
|---|---|
| `method` — вираз (`method={m}`) | поставити `<rhaix:csrf />` усередині форми вручну |
| свій `hx-headers` на елементі | додати токен туди самому: `#{"x-csrf-token": csrf.token}` |
| форма шле POST на чужий сайт | `data-no-csrf` на формі |
| публічний вебхук, який приймає POST | `csrf = false` у `[app]` — вимикає перевірку **для всього застосунку** |

Токен **лінивий**: він народжується при першій формі на сторінці. Сторінка без
форм не отримує ні токена, ні cookie.

Налаштування — секція `[app]` у `rhaix.toml`:

```toml
[app]
# Ключ підпису. Краще не тут, а у змінній оточення RHAIX_SECRET.
secret = "…"
csrf = true            # за замовчуванням увімкнено
session_cookie = "rhaix_session"
session_days = 30
tz_offset = "+03:00"   # у якому поясі показувати дати
http_timeout = 10      # секунд
```

Ключ підпису шукається в такому порядку: `RHAIX_SECRET` → `[app] secret` →
файл `.rhaix-secret` (його створює `rhaix dev`, у репозиторій він не йде) →
випадковий на час життя процесу. Останній варіант робочий, але після
перезапуску всі сесії стають недійсними — у продакшні про це буде попередження
в лозі.

---

### 7.3 `http` — виклик чужого API

```rhai
let nbu = http.get("https://bank.gov.ua/…?valcode=EUR&json");
let euro = if nbu.ok && nbu.json.len() > 0 { nbu.json[0] } else { () };
```

```rhai
http.post(url, #{ title: title })                    // мапа → JSON
http.post(url, "a=1&b=2", #{ "Content-Type": "application/x-www-form-urlencoded" })
http.get(url, #{ Authorization: `Bearer ${token}` })
http.put(url, body) / http.patch(...) / http.delete(url)
```

Виклик синхронний: ніякого `await`. Відповідь — мапа:

| Поле | Що це |
|---|---|
| `status` | код відповіді; `0`, якщо до сервера не достукались |
| `ok` | `true` для 2xx |
| `body` | тіло як текст |
| `json` | розібране тіло або `()`, якщо це не JSON |
| `headers` | мапа заголовків (імена в нижньому регістрі) |
| `error` | текст помилки або `()` |

**Помилка мережі не валить сторінку**: API лежить — буде `ok: false` і `error`,
а не 500. Тому в прикладах немає жодного `try`.

Дві речі, про які варто пам'ятати:

- Час очікування відповіді **не рахується** проти бюджету скрипта, але сторінка
  все одно чекає. Повільний API — це повільна сторінка; `http_timeout` у
  `[app]` обмежує очікування.
- Приймаються лише адреси `http://` і `https://`. Якщо адреса приходить від
  користувача, перевірте її самі: сервер піде туди зі свого боку мережі.

---

### 7.4 Дати, рядки, гроші

```rhai
now()                      // секунди від епохи
today()                    // "2026-09-17"
date(t.created)            // "2026-09-17" — розуміє і мітку часу, і рядок із бази
date(t.created, "DD.MM.YYYY о HH:mm")
datetime(now())            // "2026-09-17 14:05:09"
timestamp("2026-09-17T14:05:09Z")
now() + days(7)            // також hours(), minutes()
```

Шаблон дати — у токенах, які читаються без довідника:
`YYYY YY MM M DD D HH H mm m ss s`. Текст у подвійних лапках лишається як є
(`"о" HH:mm`). Усе інше — просто символи.

Час усередині зберігається в UTC; показ зсувається на `tz_offset` із `[app]`.
Переходу на літній час тут немає — якщо він потрібен, зона береться з бази.

```rhai
slug("Привіт, світе!")     // "pryvit-svite" — транслітерація за КМУ №55
cut(text, 20)              // обрізати по межі слова + «…»
strip_tags(text)
capitalize(text)
money(1234.5)              // "1 234,50" (нерозривний пробіл, округлення від нуля)
money(value, 0)
uuid() / random_id() / sha256(text)
hash_password(pw) / verify_password(pw, hash)   // Argon2id, сіль усередині хешу
json_encode(value) / json_decode(text)
is_blank(value)            // (), "", "   ", [], #{}
```

`json()` і `json_encode()` — різні речі: перший екранує те, що може закрити тег,
і призначений для `<script>` (2.5); другий дає звичайний рядок для бази чи API.

**Паролі — `hash_password(pw)` і `verify_password(pw, hash)`.** Це Argon2id зі
стандартними параметрами OWASP (19 МіБ, 2 проходи) і випадковою сіллю. У базу
кладуть рядок формату PHC (`$argon2id$…`) — окремої колонки для солі не треба.
`sha256()` — для контрольних сум, **не** для паролів: швидкий геш підбирається
мільярдами за секунду. `hash_password("")` повертає `()`, а `verify_password`
на зіпсованому хеші — `false`, а не помилку.

---

### 7.5 Батарейки: валідація, пагінація, файли, пошта

**`validate(значення, правила)`** — перевірка форми одним викликом замість
десятка `if`. Повертає мапу `поле → повідомлення` (порожню, якщо все гаразд):

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

Правила: `required email url int number bool date min:N max:N between:a,b len:N
same:поле different:поле in:a,b,c not_in:a,b starts:x ends:x alpha alnum`.
`min`/`max`/`between` на полі з `int`/`number` порівнюють **значення**, інакше —
**довжину** рядка. Порожнє необов'язкове поле пропускає решту правил.

**`paginate(total, per_page, page)`** — уся арифметика сторінок. `page`
затискається в межі, тож `?page=999` дає останню сторінку:

```rhai
let p = paginate(db.count("orders"), 20, req.query_int("page") ?? 1);
let rows = db.find("orders", #{}, #{ sort: "id desc", limit: 20, skip: p.skip });
```

Поля: `page pages per_page total skip from to has_prev has_next prev next
first last window` (`window` — масив номерів навколо поточного для посилань).

**Завантаження файлів.** Форма з `enctype="multipart/form-data"`:

```rhai
let f = req.file("photo");        // перше завантаження або (); ще req.files(name), req.has_file(name)
if f != () && f.is_image && f.size <= 2 * 1024 * 1024 {
    f.save(`public/uploads/${random_id()}.${f.extension}`);
}
```

Поля завантаження: `filename content_type size is_image extension`, методи
`text()` і `save(шлях)`. `save` звіряє шлях — без абсолютних і `..`, резолвиться
відносно кореня проєкту (як база). Текстові поля тієї ж форми, як звичайно, у
`req.form(...)`.

**Пошта.** `mail.send(#{ to, subject, text, html? })`:

```rhai
mail.send(#{ to: "user@example.com", subject: "Вітаємо", text: "Дякуємо." });
```

Без секції `[mail]` — dev-режим: лист друкується в лог, а не надсилається, тож
форма зворотного зв'язку працює без SMTP-сервера. Справжнє надсилання — за
feature `mail` (його вмикає `rhaix build` за наявності `[mail]`) плюс секція:

```toml
[mail]
from      = "App <noreply@example.com>"
smtp_host = "smtp.example.com"
smtp_port = 587
smtp_user = "..."
smtp_pass = "..."
```

Рецепти всіх чотирьох — у `examples/cookbook`.

### 7.6 Переклади й markdown

**`t("ключ")`.** Файли лежать у `locales/<мова>.toml` і читаються через ті самі
`Files`, що й шаблони, тож переклади працюють і у вшитому бінарнику.

```toml
# locales/uk.toml
greeting = "Привіт"
items    = "У кошику {count} товарів"
```

```rhai
t("greeting")                  // "Привіт"
t("items", #{ count: 3 })      // "У кошику 3 товарів"
set_locale("en");              // зазвичай у middleware.rhx
locale()                       // поточна мова
```

Мова за замовчуванням — `[app] locale` (типово `uk`). Пошук іде так: поточна
мова → мова за замовчуванням → **сам ключ**. Відсутній переклад видно на
сторінці як `todo.title`, а не як порожнє місце. Невідома мова в `set_locale`
ігнорується з попередженням, щоб `?lang=xx` не перетворював підписи на ключі.

**`markdown(текст)`** повертає `Html`, тобто вивід не екранується — а текст
зазвичай приходить від користувача. Тому тут два запобіжники, і вони **не
налаштовуються**:

1. **сирий HTML не проходить** — `<img src=x onerror=…>` виводиться як текст, а
   не як розмітка; у HTML потрапляють лише ті теги, які згенерував сам markdown;
2. **схема посилань перевіряється** тим самим правилом, що й атрибути:
   `[клац](javascript:alert(1))` стає `#`.

Через це санітайзер HTML не потрібен — небезпечному нема звідки взятися.
`markdown()` вмикається feature `markdown` (його ставить `rhaix build`).

---

## 8. `<style>` і `<script>` у компоненті

```html
<div class="card">…</div>

<style>
  /* у v1 — глобальний CSS, піднятий у <rhaix:head/> і дедуплікований за хешем */
  .card { border: 1px solid #ddd }
</style>

<script>
  // виконується один раз на життя сторінки, навіть якщо компонент
  // з'явився через htmx-своп кілька разів
  const data = {{ json(props) }};
  console.log("card ready", data);
</script>
```

- `<style>` **глобальний**: піднімається в `<rhaix:head/>`, дедуплікується за
  хешем вмісту.
- `<style scoped>` **звужує селектори до розмітки свого файлу**. Ядро рахує від
  шляху файлу стабільний ідентифікатор, ставить `data-rhx-<hash8>` на кожен
  елемент цього файлу й дописує `[data-rhx-…]` до **останнього** складеного
  селектора:

  ```css
  .card { }        →  .card[data-rhx-ab12cd34] { }
  .card .title { } →  .card .title[data-rhx-ab12cd34] { }
  a:hover { }      →  a[data-rhx-ab12cd34]:hover { }
  ```

  Що з цього випливає:

  - **вміст слота лишається в скоупі батька** — стиль компонента на нього не
    діє, бо розмітку писав не компонент;
  - `@media`/`@supports` обробляються всередині, а `@keyframes` і `@font-face`
    лишаються недоторканими — там не селектори;
  - `:root` у `scoped`-стилі не спрацює (він отримає атрибут): змінні теми
    тримайте у звичайному `public/*.css`;
  - у файлі зі `scoped`-стилем вимикається згортання статичних піддерев —
    рендерер має дійти до кожного елемента, щоб поставити мітку.
- `<script>` піднімається в `<rhaix:scripts/>` і виконується **один раз на життя
  сторінки**. У фрагменті він їде разом із розміткою, загорнутий у перевірку
  реєстру `rhaix.js`, тому повторний своп його не виконує.
- Теги з `src` не піднімаються: вони й так вантажаться один раз, а їхнє місце в
  документі часто важливе.
- У layout підйому немає: layout — це сам документ, його теги лишаються на місці.
- Усередині `<script>` працює лише `{{ json(x) }}` (див. 2.5); усередині `<style>`
  інтерполяція заборонена.
- **Острівців (`<script client>`) у rhaix немає.** Клієнтський інтерактив — це
  атрибути HTMX, звичайні файли `public/*.js` і hoisted `<script>` компонента.

---

## 9. Помилки

Категорії й де вони виникають:

| Етап | Приклад |
|---|---|
| Парсинг | незакритий тег, `@else` без `@if`, `{{` без `}}` |
| Резолв | невідомий компонент, циклічна залежність, невідомий слот |
| Компіляція виразу | синтаксична помилка Rhai у `{{ }}` або в `{ }` |
| Виконання | `undefined variable`, `throw`, перевищення лімітів |

Формат (dev — у консолі й overlay у браузері, прод — у лозі):

```
error: невідома змінна `todoz`
  ┌─ pages/todo.rhx:7:26
  │
7 │   <TodoItem @for={t in todoz} todo={t} />
  │                        ^^^^^ можливо, ви мали на увазі `todos`
  │
  = у ланцюжку: pages/todo.rhx → components/TodoList.rhx
```

---

## 10. Зарезервовано

- Префікс тегів `rhaix:` — тільки для ядра.
- Атрибути, що починаються з `@` — тільки директиви.
- Атрибути `data-rhx-*` — службові.
- Імена компонентів `Fragment`, `Slot` — службові.
- **[v1.1]**: scoped-слоти, `@key` для morph-свопів, `@transition`.
- **Не планується**: острівці (`<script client>`), часткова гідратація, клієнтські
  компоненти — це свідомо поза межами фреймворку.

---

## 11. Граматика (EBNF, спрощено)

```ebnf
file         = [ frontmatter ] , nodes ;
frontmatter  = "---" , NL , rhai_code , NL , "---" , NL ;

nodes        = { node } ;
node         = text | interp | comment | raw_block | element | component ;

text         = { CHAR - "{{" - "<" } ;
interp       = "{{" , [ "-" ] , rhai_expr , [ "-" ] , "}}" ;
comment      = "{{!" , { CHAR } , "}}" ;
raw_block    = "<rhaix:raw>" , { CHAR } , "</rhaix:raw>" ;

element      = "<" , html_name , { attribute } , ( "/>" | ">" , nodes , "</" , html_name , ">" ) ;
component    = "<" , comp_name , { attribute } , ( "/>" | ">" , nodes , "</" , comp_name , ">" ) ;

html_name    = LOWER , { LETTER | DIGIT | "-" } ;
comp_name    = UPPER , { LETTER | DIGIT | "_" } , { "." , ( UPPER | LOWER ) , { LETTER | DIGIT } } ;

attribute    = directive | spread | named_attr ;
directive    = "@" , dir_name , [ "=" , "{" , rhai_expr , "}" ] ;
spread       = "{" , "..." , rhai_expr , "}" ;
named_attr   = attr_name , [ "=" , attr_value ] ;
attr_value   = '"' , { text | interp } , '"'
             | "'" , { text | interp } , "'"
             | "{" , rhai_expr , "}"
             | unquoted ;
dir_name     = "if" | "else-if" | "else" | "for" | "key"
             | "class" | "style" | "attr" | "html" | "text" | "oob" ;
```

Void-елементи (`<br>`, `<img>`, `<input>`, `<meta>`, `<link>`, `<hr>`, `<source>`,
`<area>`, `<base>`, `<col>`, `<embed>`, `<track>`, `<wbr>`) не потребують закриття.
Вміст `<script>`, `<style>`, `<pre>`, `<textarea>` не парситься як розмітка
(але `{{ }}` у `<script>`/`<style>` **працює** — це навмисно, щоб передавати дані
в JS: `const id = {{ todo.id }};`).

---

## 12. Повний приклад застосунку

```
pages/todo.rhx
---
let todos = db.query("select id, title, done from todos order by id");
page.title = "ToDo";
---
<h1>ToDo</h1>
<form hx-post="/api/todo" hx-target="#list" hx-swap="outerHTML"
      hx-on::after-request="this.reset()">
  <input name="title" placeholder="Що зробити?" required>
  <button>Додати</button>
</form>
<TodoList todos={todos} />
```

```
components/TodoList.rhx
---
let todos = props.todos ?? [];
---
<ul id="list">
  <TodoItem @for={t in todos} @key={t.id} todo={t} />
  <li @if={todos.is_empty()} class="empty">Порожньо</li>
</ul>
```

```
components/TodoItem.rhx
---
let todo = props.todo;
---
<li class="todo-item" @class={#{"completed": todo.done}}>
  <input type="checkbox" @attr={#{"checked": todo.done}}
         hx-patch="/api/todo?id={{ todo.id }}" hx-target="#list" hx-swap="outerHTML">
  <span>{{ todo.title }}</span>
  <button hx-delete="/api/todo?id={{ todo.id }}" hx-target="#list" hx-swap="outerHTML">×</button>
</li>

<style>
  .todo-item.completed span { text-decoration: line-through; opacity: .6 }
</style>
```

```
pages/api/todo.rhx
---
if req.method == "POST" {
  db.exec("insert into todos(title, done) values(?, 0)", [req.form("title")]);
  hx.toast("Додано", "success");
} else if req.method == "PATCH" {
  db.exec("update todos set done = not done where id = ?", [req.query("id")]);
} else if req.method == "DELETE" {
  db.exec("delete from todos where id = ?", [req.query("id")]);
  hx.toast("Видалено", "info");
}
let todos = db.query("select id, title, done from todos order by id");
---
<TodoList todos={todos} />
```

4 файли, нуль Rust, нуль збірки фронтенду.
