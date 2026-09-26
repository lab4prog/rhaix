# Кухарська книга rhaix

Кожен файл — відповідь на одну задачу, з поясненням у коментарі на початку.
Запустити (сервер підніметься на `http://localhost:3100`):

```bash
rhaix dev examples/cookbook
```

У клоні репозиторію без встановлення — `cargo run -p rhaix-cli -- dev examples/cookbook`.

| Задача | Файл |
|---|---|
| Список із бази з фільтром і сортуванням по колонці | [`pages/table.rhx`](pages/table.rhx) |
| Форма з валідацією, помилками біля полів і записом у базу | [`pages/form.rhx`](pages/form.rhx) |
| Модальне вікно з карткою запису | [`pages/modal.rhx`](pages/modal.rhx) + [`partials/OrderCard.rhx`](partials/OrderCard.rhx) |
| Живий пошук під час набору | [`pages/search.rhx`](pages/search.rhx) + [`partials/OrderRows.rhx`](partials/OrderRows.rhx) |
| Пагінація з робочими посиланнями | [`pages/pagination.rhx`](pages/pagination.rhx) |
| Спільні функції для всіх сторінок | [`scripts/orders.rhai`](scripts/orders.rhai) |
| JSON-API поверх тих самих даних | [`api/orders.rhx`](api/orders.rhx) + [`api/orders/[id].rhx`](api/orders/[id].rhx) |
| Доступ до API за токеном | [`middleware.rhx`](middleware.rhx) |
| Ролі й права: охорона шляхів і приховані кнопки | [`middleware.rhx`](middleware.rhx) + [`scripts/access.rhai`](scripts/access.rhai) + [`pages/roles.rhx`](pages/roles.rhx) |
| Адмін-таблиця: сортування, фільтри, сторінки (`db.grid`) | [`pages/grid.rhx`](pages/grid.rhx) |
| Живі оновлення: сторінка оновлюється сама (`live.send`) | [`pages/live.rhx`](pages/live.rhx) |
| Вивантаження в CSV для Excel | [`pages/reports.rhx`](pages/reports.rhx) + [`pages/reports/export.rhx`](pages/reports/export.rhx) |
| Завантаження файлу з перевіркою типу й розміру | [`pages/upload.rhx`](pages/upload.rhx) |
| Форма зворотного зв'язку, що надсилає лист | [`pages/contact.rhx`](pages/contact.rhx) |
| Ліміт запитів до API за адресою й за токеном | [`middleware.rhx`](middleware.rhx) |

Три речі, які повторюються в усіх рецептах:

1. **Про CSRF немає жодного рядка.** Форма отримує приховане поле, кнопка з
   `hx-delete` — заголовок; це робить фреймворк.
2. **У SQL нічого не склеюється руками.** Фільтри — це мапи
   (`#{ customer: #{ contains: q } }`), а значення з адреси, що йде в `order by`,
   звіряється зі списком дозволених колонок.
3. **Фрагмент нічого не знає про сторінку.** `partials/OrderRows.rhx` віддає
   самі `<tr>`; куди їх вставити — вирішує `hx-target` на сторінці.

## API

Ті самі замовлення, але для машин. Токен із міграції — `demo-token-42`:

```bash
curl -H "Authorization: Bearer demo-token-42" http://localhost:3100/api/orders
```

```bash
curl -X POST http://localhost:3100/api/orders -H "Authorization: Bearer demo-token-42" -H "Content-Type: application/json" -d '{"customer":"Нова Клієнтка","email":"n@example.com","amount":250.5}'
```

Чим `api/` відрізняється від `pages/`:

- `return #{ ... }` стає JSON **сам** — ні `json_encode`, ні `page.layout = false`,
  ні `res.header("content-type", ...)` писати не треба;
- CSRF не перевіряється, бо **сесії там немає взагалі**. Автентифікація можлива
  лише за токеном із заголовка, тож чужий сайт не може послати запит від імені
  залогіненого користувача — його cookie просто не читають;
- помилки, 404 і діагностика теж приїжджають JSON-ом: клієнт ніколи не отримає
  HTML там, де чекав дані;
- `middleware.rhx` обмежує запити двічі: 120 на хвилину з однієї адреси (ще до
  перевірки токена, щоб його не можна було підбирати) і 60 на хвилину на
  токен. Понад ліміт — `429` із заголовком `Retry-After`.

Рецепти перевіряються тестом `crates/rhaix-server/tests/examples.rs`: зламаний
рецепт валить збірку. Сам API — `crates/rhaix-server/tests/api.rs`, і там він
проганяється живими запитами, а не лише компілюється.
