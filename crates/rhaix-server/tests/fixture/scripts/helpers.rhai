// Спільні функції для сторінок. Підключається через `import "helpers" as h;`.

fn vat(amount) {
    amount * 1.2
}

fn label(count) {
    if count == 1 { "запис" } else { "записів" }
}
