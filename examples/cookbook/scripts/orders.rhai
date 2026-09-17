// Спільні функції проєкту. Підключати нічого не треба:
// усе, що тут оголошено, видно з будь-якого `.rhx`.

/// Людська назва статусу.
fn status_label(status) {
    switch status {
        "new"       => "Новий",
        "paid"      => "Оплачено",
        "cancelled" => "Скасовано",
        _           => status,
    }
}

/// Клас для кольорової плашки.
fn status_class(status) {
    `badge badge-${status}`
}
