# Кухарська книга rhaix

Кожен файл — відповідь на одну задачу. Запустити:

```bash
cargo run -p rhaix-cli -- dev examples/cookbook
```

| Задача | Файл |
|---|---|
| Список із бази з фільтром і сортуванням по колонці | [`pages/table.rhx`](pages/table.rhx) |
| Форма з валідацією, помилками біля полів і записом у базу | [`pages/form.rhx`](pages/form.rhx) |
| Модальне вікно з карткою запису | [`pages/modal.rhx`](pages/modal.rhx) + [`partials/OrderCard.rhx`](partials/OrderCard.rhx) |
| Живий пошук під час набору | [`pages/search.rhx`](pages/search.rhx) + [`partials/OrderRows.rhx`](partials/OrderRows.rhx) |
| Пагінація з робочими посиланнями | [`pages/pagination.rhx`](pages/pagination.rhx) |
| Спільні функції для всіх сторінок | [`scripts/orders.rhai`](scripts/orders.rhai) |

Три речі, які повторюються в усіх рецептах:

1. **Про CSRF немає жодного рядка.** Форма отримує приховане поле, кнопка з
   `hx-delete` — заголовок; це робить фреймворк.
2. **У SQL нічого не склеюється руками.** Фільтри — це мапи
   (`#{ customer: #{ contains: q } }`), а значення з адреси, що йде в `order by`,
   звіряється зі списком дозволених колонок.
3. **Фрагмент нічого не знає про сторінку.** `partials/OrderRows.rhx` віддає
   самі `<tr>`; куди їх вставити — вирішує `hx-target` на сторінці.

Рецепти перевіряються тестом `crates/rhaix-server/tests/examples.rs`: зламаний
рецепт валить збірку.
