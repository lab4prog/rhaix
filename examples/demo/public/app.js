// Прототип майбутнього rhaix.js (M7): тости з HX-Trigger і реєстр асетів.
// Саме те, що в Node-RED-стартері доводилось писати в main.js руками.
document.body.addEventListener("showToast", (event) => {
  const { message, type } = event.detail ?? {};
  const el = document.createElement("div");
  el.className = `toast ${type ?? "info"}`;
  el.textContent = decodeURIComponent(message ?? "");
  document.getElementById("toasts")?.appendChild(el);
  setTimeout(() => el.remove(), 3000);
});
