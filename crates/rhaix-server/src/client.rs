//! `rhaix.js` — крихітний клієнт, який ядро віддає саме.
//!
//! Це той самий код, який у Node-RED-стартері доводилось тримати в `main.js`
//! і копіювати з проєкту в проєкт: тости з `HX-Trigger` і реєстр уже
//! завантажених асетів. Різниця в тому, що тут він приїжджає з фреймворком.

/// Шлях, за яким віддається клієнт.
pub const CLIENT_ROUTE: &str = "/_rhaix/rhaix.js";

/// Шлях, за яким віддається htmx.
pub const HTMX_ROUTE: &str = "/_rhaix/htmx.js";

/// htmx, вшитий у бінарник.
///
/// Інакше обіцянка «деплой — це копіювання одного файлу» була б неправдою:
/// сторінка не працювала б без інтернету й чужого CDN. 51 КБ у бінарнику —
/// чесна ціна за застосунок, який справді самодостатній.
///
/// htmx 2.0.7, ліцензія Zero-Clause BSD (дозволяє використання й поширення
/// без умов). Джерело: https://htmx.org
pub const HTMX_JS: &str = include_str!("../vendor/htmx.min.js");

/// Вміст `rhaix.js`.
pub const CLIENT_JS: &str = r##"(() => {
  const seen = new Set();

  // `window.__rhaix` — не перезаписуємо, а доповнюємо: якщо `public/*.js`
  // (він підключається ПІСЛЯ цього файлу) ще не встиг нічого покласти сюди,
  // об'єкт однаково має існувати вже зараз, до першого доступу до нього.
  window.__rhaix = window.__rhaix || {};

  // htmx за замовчуванням свопить лише 2xx (config.responseHandling) і мовчки
  // викидає решту. У rhaix `res.status(...)` — це частина звичайної відповіді,
  // не сигнал «щось не так»: 422 несе ту саму сторінку з помилками під
  // полями (SYNTAX 7.5), 403 — пояснення, 404 — готову сторінку. Без цього
  // рядка `validate()` на сервері відпрацьовує правильно, а користувач не
  // бачить жодної помилки — клік просто «нічого не робить».
  htmx.config.responseHandling = [{ code: "...", swap: true }];

  // Реєстр асетів: піднятий <script> компонента виконується один раз на
  // життя сторінки, навіть якщо компонент приїхав ще кілька разів фрагментом.
  window.__rhaix.seen = window.__rhaix.seen || function (hash) {
    if (seen.has(hash)) return true;
    seen.add(hash);
    return false;
  };

  // Тости: сервер шле подію заголовком HX-Trigger (`hx.toast(...)`), клієнт
  // її малює. Сам малюнок — окрема функція, а не логіка всередині
  // слухача, — щоб її можна було замінити, не чіпаючи подію:
  //
  //   // public/app.js, підключається ПІСЛЯ rhaix.js — просто перевизначте:
  //   window.__rhaix.toast = (message, type) => myToastLib.show(message, type);
  //
  // Перевизначення читається щоразу під час самої події, а не запам'ятовується
  // наперед, тому працює незалежно від того, коли саме app.js це зробив.
  window.__rhaix.toast = window.__rhaix.toast || function (message, type) {
    const box = document.getElementById("toasts");
    if (!box) return;
    const toast = document.createElement("div");
    toast.className = `toast ${type ?? "info"}`;
    toast.textContent = message ?? "";
    box.appendChild(toast);
    setTimeout(() => toast.remove(), 3000);
  };
  document.body.addEventListener("showToast", (event) => {
    const detail = event.detail ?? {};
    window.__rhaix.toast(detail.message, detail.type);
  });

  // <dialog>, де б він не з'явився — у першому завантаженні чи в htmx-фрагменті
  // (звичайному чи oob), — відкривається як справжній модал: нативний backdrop,
  // Esc, фокус-пастка, без жодного рядка коду в проєкті. Досить писати
  // `<dialog>` замість `<div class="modal">`. Закривається сам, щойно елемент
  // прибирають зі сторінки (порожня відповідь на те саме `hx-target` — типовий
  // спосіб закрити діалог, SYNTAX 4.7).
  //
  // Проєкту, якому потрібен саме нейтральний, немодальний `<dialog>`, досить
  // додати `data-plain` — тоді ця функція його не чіпає.
  //
  // `htmx:load` — єдина подія, що покриває і початкове завантаження, і кожен
  // своп, і oob-свопи однаково (офіційна заміна htmx:afterProcessNode).
  document.body.addEventListener("htmx:load", (event) => {
    const root = event.detail?.elt ?? document;
    const dialogs = root.matches?.("dialog") ? [root] : [];
    dialogs.push(...(root.querySelectorAll?.("dialog") ?? []));
    for (const dialog of dialogs) {
      if (dialog.hasAttribute("data-rhx-modal") || dialog.hasAttribute("data-plain")) continue;
      dialog.removeAttribute("open"); // showModal() сам відкриє й додасть top-layer
      dialog.setAttribute("data-rhx-modal", "");
      dialog.showModal();
    }
  });

  // Стилі, що приїхали разом із фрагментом, можуть повторювати вже наявні:
  // лишаємо перший, решту прибираємо, щоб документ не ріс на кожному свопі.
  document.addEventListener("htmx:afterSwap", () => {
    const styles = document.querySelectorAll("style[data-rhx]");
    const kept = new Set();
    for (const style of styles) {
      const hash = style.getAttribute("data-rhx");
      if (kept.has(hash)) style.remove();
      else kept.add(hash);
    }
  });
})();
"##;

/// `<style>` компонента у вигляді тега з міткою.
pub fn style_tag(hash: &str, body: &str) -> String {
    format!("<style data-rhx=\"{hash}\">{body}</style>")
}

/// `<script>` компонента, загорнутий у перевірку реєстру.
///
/// Саме ця обгортка робить дедуплікацію для фрагментів: htmx виконує скрипти
/// у вставленому HTML, тому без неї код компонента виконувався б знову на
/// кожному свопі.
pub fn script_tag(hash: &str, body: &str) -> String {
    format!("<script data-rhx=\"{hash}\">if(!window.__rhaix||!window.__rhaix.seen(\"{hash}\")){{{body}}}</script>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_is_wrapped_in_the_registry_check() {
        let tag = script_tag("abc123", "console.log(1);");
        assert!(tag.contains("data-rhx=\"abc123\""), "{tag}");
        assert!(tag.contains("__rhaix.seen(\"abc123\")"), "{tag}");
        assert!(tag.contains("console.log(1);"), "{tag}");
    }

    #[test]
    fn client_defines_the_registry_before_anything_else() {
        assert!(CLIENT_JS.contains("window.__rhaix"), "реєстр є");
        assert!(CLIENT_JS.contains("showToast"), "тости є");
    }

    #[test]
    fn error_status_responses_still_swap() {
        // За замовчуванням htmx свопить лише 2xx і мовчки викидає решту —
        // без цього `res.status(422)` із помилками під полями чи `403` з
        // поясненням ніколи не з'являються на екрані.
        assert!(CLIENT_JS.contains("htmx.config.responseHandling"));
        assert!(CLIENT_JS.contains(r#"code: "...", swap: true"#));
    }

    #[test]
    fn toast_rendering_is_an_overridable_hook() {
        // Малюнок — окрема функція на `window.__rhaix.toast`, а не логіка
        // всередині слухача: інакше проєкту не було б за що зачепитись, щоб
        // намалювати тост власним компонентом.
        assert!(CLIENT_JS.contains("window.__rhaix.toast = window.__rhaix.toast || function"));
        // `||` — не перезаписати те, що вже поклав public/*.js.
        assert!(CLIENT_JS.contains("window.__rhaix = window.__rhaix || {}"));
    }

    #[test]
    fn dialog_elements_are_opened_as_native_modals() {
        assert!(
            CLIENT_JS.contains("htmx:load"),
            "єдина подія на всі шляхи появи"
        );
        assert!(CLIENT_JS.contains("showModal()"));
        assert!(
            CLIENT_JS.contains("data-plain"),
            "має бути шлях відмовитись від автомодалу"
        );
    }
}
