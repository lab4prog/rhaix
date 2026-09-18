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

  // Реєстр асетів: піднятий <script> компонента виконується один раз на
  // життя сторінки, навіть якщо компонент приїхав ще кілька разів фрагментом.
  window.__rhaix = {
    seen(hash) {
      if (seen.has(hash)) return true;
      seen.add(hash);
      return false;
    },
  };

  // Тости: сервер шле подію заголовком HX-Trigger, клієнт її показує.
  document.addEventListener("DOMContentLoaded", () => {
    document.body.addEventListener("showToast", (event) => {
      const detail = event.detail ?? {};
      const box = document.getElementById("toasts");
      if (!box) return;

      const toast = document.createElement("div");
      toast.className = `toast ${detail.type ?? "info"}`;
      toast.textContent = detail.message ?? "";
      box.appendChild(toast);
      setTimeout(() => toast.remove(), 3000);
    });
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
}
