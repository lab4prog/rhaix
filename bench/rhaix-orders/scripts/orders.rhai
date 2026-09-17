fn status_label(status) {
    switch status {
        "new"       => "Новий",
        "paid"      => "Оплачено",
        "cancelled" => "Скасовано",
        _           => status,
    }
}

fn status_class(status) {
    `badge badge-${status}`
}
