// ui.js — типовий інтерфейс rhaix: тости й модальні вікна.
//
// Це НЕ ядро фреймворку, а його типова реалізація, яку можна замінити:
//
//   - точково: перевизначте одну функцію у своєму public/*.js
//       window.__rhaix.toast = (message, type) => myToasts.show(message, type);
//
//   - повністю: `rhaix eject ui` кладе копію цього файлу в public/rhaix-ui.js.
//     Щойно такий файл є, фреймворк підключає його ЗАМІСТЬ /_rhaix/ui.js —
//     правте як хочете, навіть очистьте, щоб вимкнути все це.
//
// Контракт із сервером, від якого ця реалізація залежить (і який має
// виконувати будь-яка заміна):
//   - подія `showToast` (з HX-Trigger, `hx.toast(message, type)` на сервері),
//     detail: { message, type } або { message, type, items: [{message, type}, …] }
//     — якщо за один запит було кілька `hx.toast(...)`;
//   - `<script type="application/json" data-rhx-toasts>[{message, type}, …]`
//     у повній сторінці — тости, що пережили редірект (flash);
//   - `<dialog>` у будь-якій відповіді — модальне вікно.
//
// Підключається в <head> синхронно, до <body>: слухачі — на `document`,
// контейнер тостів створюється лише тоді, коли тост справді прийшов.
(() => {
  const rhaix = (window.__rhaix = window.__rhaix || {});
  if (rhaix.uiLoaded) return;
  rhaix.uiLoaded = true;

  // Типові стилі з нульовою специфічністю (`:where`): будь-яке правило
  // проєкту їх перекриває, навіть просте `.toast { … }`. Без них тости в
  // проєкті без власного CSS були б невидимим текстом у кутку.
  const style = document.createElement("style");
  style.setAttribute("data-rhx-ui", "");
  style.textContent = `
:where(#toasts, .rhx-toasts) { position: fixed; right: 1rem; bottom: 1rem; z-index: 1000;
  display: grid; gap: .5rem; max-width: min(24rem, calc(100vw - 2rem)) }
:where(#toasts, .rhx-toasts) > :where(.toast) { padding: .6rem 1rem; border-radius: 6px;
  color: #fff; background: #334155; font: 14px/1.4 system-ui, sans-serif; cursor: pointer;
  box-shadow: 0 4px 12px rgb(0 0 0 / .15) }
:where(#toasts, .rhx-toasts) > :where(.toast.success) { background: #15803d }
:where(#toasts, .rhx-toasts) > :where(.toast.error) { background: #b91c1c }
:where(#toasts, .rhx-toasts) > :where(.toast.warning) { background: #b45309 }
:where(dialog[data-rhx-modal])::backdrop { background: rgb(0 0 0 / .4) }`;
  (document.head || document.documentElement).appendChild(style);

  // Скільки тост висить на екрані, мс. Можна змінити в public/*.js.
  if (rhaix.toastTimeout === undefined) rhaix.toastTimeout = 4000;

  // Контейнер: `#toasts` із layout, якщо він є (так робили до 1.2.5), інакше
  // створюємо свій. Шукаємо щоразу заново — boosted-перехід міг замінити <body>.
  function toastBox() {
    let box = document.getElementById("toasts");
    if (!box) {
      box = document.createElement("div");
      box.id = "toasts";
      document.body.appendChild(box);
    }
    box.classList.add("rhx-toasts");
    box.setAttribute("role", "status");
    box.setAttribute("aria-live", "polite");
    return box;
  }

  // Один тост. Перевизначте цю функцію, щоб малювати тости по-своєму —
  // подія й сервер лишаються ті самі.
  rhaix.toast = rhaix.toast || function (message, type) {
    const toast = document.createElement("div");
    toast.className = `toast ${type || "info"}`;
    toast.textContent = message ?? "";
    toast.addEventListener("click", () => toast.remove());
    toastBox().appendChild(toast);
    if (rhaix.toastTimeout > 0) setTimeout(() => toast.remove(), rhaix.toastTimeout);
  };

  document.addEventListener("showToast", (event) => {
    const detail = event.detail ?? {};
    const items = Array.isArray(detail.items) ? detail.items : [detail];
    // Функцію читаємо в момент події, а не запам'ятовуємо: перевизначення з
    // public/*.js діє, хоч би коли воно відбулося.
    for (const item of items) rhaix.toast(item.message, item.type);
  });

  // Тости, що пережили редірект: сервер кладе їх у сторінку, бо HX-Trigger на
  // відповіді з редіректом htmx малює — і одразу йде на нову адресу.
  document.addEventListener("htmx:load", (event) => {
    const root = event.detail?.elt ?? document;
    for (const carrier of root.querySelectorAll?.("script[data-rhx-toasts]") ?? []) {
      let items = [];
      try { items = JSON.parse(carrier.textContent); } catch { /* зіпсоване — пропускаємо */ }
      carrier.remove();
      for (const item of items) rhaix.toast(item.message, item.type);
    }
  });

  // <dialog>, де б він не з'явився — у першому завантаженні, у фрагменті, в
  // oob, — відкривається як справжній модал: нативний backdrop, Esc,
  // фокус-пастка. Закривається сам, щойно елемент прибирають зі сторінки
  // (порожня відповідь на той самий hx-target). `data-plain` — лишити
  // звичайним немодальним <dialog>.
  //
  // `htmx:load` покриває всі шляхи появи однаково.
  document.addEventListener("htmx:load", (event) => {
    const root = event.detail?.elt ?? document;
    const dialogs = root.matches?.("dialog") ? [root] : [];
    dialogs.push(...(root.querySelectorAll?.("dialog") ?? []));
    for (const dialog of dialogs) {
      if (dialog.hasAttribute("data-rhx-modal") || dialog.hasAttribute("data-plain")) continue;
      if (!dialog.isConnected) continue;
      dialog.removeAttribute("open"); // showModal() відкриває сам; з `open` він кидає помилку
      dialog.setAttribute("data-rhx-modal", "");
      dialog.showModal();
    }
  });
})();
