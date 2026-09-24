// rhaix.js — ядро клієнта. Лише протокол між сервером і htmx, жодного UI.
//
// Тости, модалки й решта того, що видно користувачеві, живуть окремо — у
// /_rhaix/ui.js, який проєкт може повністю замінити своїм (`rhaix eject ui`).
// Цей файл замінювати не треба: без нього ламаються 422-помилки під полями,
// дедуплікація стилів компонентів і реєстр їхніх скриптів.
//
// Підключається в <head> синхронно, до <body>, тому:
//   - слухачі вішаються на `document`, а не на `document.body` (його ще немає);
//   - htmx свопить лише <body>, тож цей файл ніколи не виконається вдруге —
//     але охорона нижче все одно стоїть: layout без <rhaix:head/> кладе його
//     в кінець <body>.
(() => {
  const rhaix = (window.__rhaix = window.__rhaix || {});
  if (rhaix.coreLoaded) return;
  rhaix.coreLoaded = true;

  // Реєстр асетів: піднятий <script> компонента виконується один раз на
  // життя сторінки, навіть якщо компонент приїхав ще кілька разів фрагментом.
  // Зазвичай його вже визначив інлайн-скрипт у <head>; тут — запасний шлях.
  if (!rhaix.seen) {
    const seen = new Set();
    rhaix.seen = (hash) => seen.has(hash) || (seen.add(hash), false);
  }

  // htmx за замовчуванням свопить лише 2xx і мовчки викидає решту. У rhaix
  // `res.status(...)` — частина звичайної відповіді, а не сигнал «щось не
  // так»: 422 несе ту саму форму з помилками під полями, 403 — пояснення,
  // 404/500 — готову сторінку. `error: true` лишає htmx:responseError для
  // тих, хто його слухає; 204 не свопиться — тіла там немає, і порожній своп
  // стер би ціль. 422 — штатне «форма з помилками» (validate()), а не збій:
  // без `error` htmx не пише його в консоль як помилку на кожну валідацію.
  htmx.config.responseHandling = [
    { code: "204", swap: false },
    { code: "[23]..", swap: true },
    { code: "422", swap: true },
    { code: "[45]..", swap: true, error: true },
  ];

  // Стилі, що приїхали разом із фрагментом, можуть повторювати вже наявні:
  // лишаємо перший, решту прибираємо, щоб документ не ріс на кожному свопі.
  document.addEventListener("htmx:afterSwap", () => {
    const kept = new Set();
    for (const style of document.querySelectorAll("style[data-rhx]")) {
      const hash = style.getAttribute("data-rhx");
      if (kept.has(hash)) style.remove();
      else kept.add(hash);
    }
  });

  // Живі оновлення (`live.send("orders")` на сервері). Сторінка слухає теми
  // звичайним htmx: `hx-trigger="live:orders from:body"` — а тут одна
  // SSE-підписка на всі теми, які згадані на сторінці (у `hx-trigger` або в
  // `data-live="orders users"` для власного JS). Подія приходить на <body>
  // як `live:<тема>` з detail від сервера.
  let source = null;
  let subscribed = "";
  let dropped = false;

  function topics() {
    const found = new Set();
    for (const el of document.querySelectorAll('[hx-trigger*="live:"], [data-live]')) {
      const trigger = el.getAttribute("hx-trigger") || "";
      for (const match of trigger.matchAll(/live:([\w.-]+)/g)) found.add(match[1]);
      for (const name of (el.getAttribute("data-live") || "").split(/[\s,]+/)) {
        if (name) found.add(name);
      }
    }
    return [...found].sort();
  }

  function fire(topic, detail) {
    document.body.dispatchEvent(
      new CustomEvent(`live:${topic}`, { detail: detail ?? {}, bubbles: true })
    );
  }

  // Після свопу набір тем міг змінитись — перепідписуємось лише тоді.
  function subscribe() {
    const list = topics();
    const key = list.join(",");
    if (key === subscribed) return;
    subscribed = key;
    if (source) source.close();
    source = null;
    if (!list.length) return;
    source = new EventSource(`/_rhaix/live?topics=${encodeURIComponent(key)}`);
    source.onmessage = (event) => {
      let message;
      try { message = JSON.parse(event.data); } catch { return; }
      fire(message.topic, message.detail);
    };
    // Поки з'єднання не було, події могли пройти повз. EventSource
    // перепідключається сам; ми лише кажемо всім темам «перезапитай».
    source.onerror = () => { dropped = true; };
    source.onopen = () => {
      if (!dropped) return;
      dropped = false;
      for (const topic of list) fire(topic, { reconnected: true });
    };
  }
  rhaix.liveTopics = topics;

  // `htmx:load` приходить і на першому завантаженні, і на кожному свопі.
  document.addEventListener("htmx:load", subscribe);
})();
