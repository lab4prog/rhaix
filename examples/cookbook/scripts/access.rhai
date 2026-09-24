// Ролі й права — одна таблиця на весь застосунок.
//
// Права, а не ролі, — ось що перевіряють сторінки: `can(user, "orders.export")`,
// а не `user.role == "admin" || user.role == "manager"`. Тоді нова роль —
// це новий рядок тут, а не пошук по всіх файлах, де згадано стару.

/// Що дозволено ролі.
fn permissions(role) {
    switch role {
        "admin"   => ["orders.view", "orders.export", "orders.cancel"],
        "manager" => ["orders.view", "orders.export"],
        "viewer"  => ["orders.view"],
        _         => [],
    }
}

/// Чи може користувач (запис із бази або `()`) зробити дію.
fn can(user, permission) {
    if user == () { return false; }
    permissions(user.role).contains(permission)
}

/// Людська назва ролі — для меню й сторінки вибору.
fn role_label(role) {
    switch role {
        "admin"   => "адміністратор",
        "manager" => "менеджер",
        "viewer"  => "переглядач",
        _         => role,
    }
}
