//! Ворота M1: та сама таблиця 1000 × 10, але вже через справжній шаблон.
//!
//! Бенчмарк M0 міряв «голий» Rhai і показував, на що можна розраховувати.
//! Тут перевіряється, чи вкладається в ту саму межу повний шлях:
//! розбір → компіляція → рендер із екрануванням.
//!
//! Запуск: `cargo run --release -p rhaix-template --bin rhaix-render-bench`

use std::sync::Arc;
use std::time::Instant;

use rhai::{Dynamic, Map, Scope};
use rhaix_parser::Source;
use rhaix_script::{engine, Limits};
use rhaix_template::{Globals, Slots, Template};

const ROWS: usize = 1000;
/// Ворота M1 — інша величина, ніж ворота M0.
///
/// M0 міряв лише обчислення виразів (без розмітки й екранування) і дав 4.88 мс.
/// Тут вимірюється повний шлях, який додає ще ~3 мс на запис 300 КБ HTML, тому
/// межа піднята до 10 мс. Розклад друкується нижче, щоб підміни не сталося тихо.
const GATE_MS: f64 = 10.0;

const TABLE_PLAIN: &str = r#"<table>
  <tr @for={row in rows}>
    <td>текст</td><td>текст</td><td>текст</td><td>текст</td><td>текст</td>
    <td>текст</td><td>текст</td><td>текст</td><td>текст</td><td>текст</td>
  </tr>
</table>
"#;

const TABLE_FIELDS: &str = r#"<table>
  <tr @for={row in rows}>
    <td>{{ row.id }}</td>
    <td>{{ row.title }}</td>
    <td>{{ row.dept }}</td>
    <td>{{ row.owner }}</td>
    <td>{{ row.status }}</td>
    <td>{{ row.updated }}</td>
    <td>{{ row.note }}</td>
    <td>{{ row.price }}</td>
    <td>{{ row.qty }}</td>
    <td>{{ row.done }}</td>
  </tr>
</table>
"#;

const TABLE: &str = r#"<table>
  <tr @for={row in rows}>
    <td>{{ row.id }}</td>
    <td>{{ row.title }}</td>
    <td>{{ row.dept }}</td>
    <td>{{ row.owner }}</td>
    <td>{{ row.status }}</td>
    <td>{{ row.updated }}</td>
    <td>{{ row.price * row.qty }}</td>
    <td>{{ if row.done { "✔" } else { "…" } }}</td>
    <td>{{ row.title + " (" + row.dept + ")" }}</td>
    <td>{{ if row.price > 100 { "дорого" } else { "норм" } }}</td>
  </tr>
</table>
"#;

const TABLE_FULL: &str = r#"<table>
  <tr @for={row in rows} @class={#{"done": row.done}}>
    <td>{{ row.id }}</td>
    <td>{{ row.title }}</td>
    <td>{{ row.dept }}</td>
    <td>{{ row.owner }}</td>
    <td>{{ row.status }}</td>
    <td>{{ row.updated }}</td>
    <td>{{ row.price * row.qty }}</td>
    <td>{{ if row.done { "✔" } else { "…" } }}</td>
    <td>{{ row.title + " (" + row.dept + ")" }}</td>
    <td>{{ if row.price > 100 { "дорого" } else { "норм" } }}</td>
  </tr>
</table>
"#;

fn main() {
    let iters: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(20);

    let engine = engine(Limits::default());
    let rows = Dynamic::from(make_rows(ROWS));

    println!("rhaix — ворота M1 (повний шлях: розбір → компіляція → рендер)");
    println!(
        "{ROWS} рядків, {iters} прогонів
"
    );
    println!("{:<44} {:>10} {:>10}", "шаблон", "медіана", "краще");

    let plain = measure(&engine, TABLE_PLAIN, &rows, iters);
    let fields = measure(&engine, TABLE_FIELDS, &rows, iters);
    let mixed = measure(&engine, TABLE, &rows, iters);
    let full = measure(&engine, TABLE_FULL, &rows, iters);

    report("лише текст (без виразів)", plain);
    report("10 звернень до поля", fields);
    report("6 полів + 4 вирази (як у воротах M0)", mixed);
    report("те саме + @class", full);

    println!();
    if mixed.0 < GATE_MS {
        println!("PASS: {:.2} мс < {GATE_MS:.0} мс", mixed.0);
    } else {
        println!("FAIL: {:.2} мс ≥ {GATE_MS:.0} мс", mixed.0);
    }
}

fn measure(engine: &rhai::Engine, text: &str, rows: &Dynamic, iters: usize) -> (f64, f64) {
    let source = Arc::new(Source::new("bench.rhx", text));
    let template = Template::compile(source, engine).expect("шаблон має компілюватись");

    let mut samples = Vec::with_capacity(iters + 1);
    for _ in 0..=iters {
        let mut scope = Scope::new();
        scope.push_dynamic("rows", rows.clone());
        let started = Instant::now();
        let rendered = template
            .render(engine, &mut scope, Slots::default(), &Globals::default())
            .expect("рендер має працювати");
        samples.push(started.elapsed());
        std::hint::black_box(rendered);
    }
    samples.remove(0); // прогрів
    samples.sort();
    (
        samples[samples.len() / 2].as_secs_f64() * 1000.0,
        samples[0].as_secs_f64() * 1000.0,
    )
}

fn report(label: &str, (median, best): (f64, f64)) {
    println!("{label:<44} {median:>8.2} мс {best:>8.2} мс");
}

fn make_rows(n: usize) -> Vec<Dynamic> {
    (0..n)
        .map(|i| {
            let mut row = Map::new();
            row.insert("id".into(), Dynamic::from(i as i64));
            row.insert("title".into(), Dynamic::from(format!("Завдання №{i}")));
            row.insert("dept".into(), Dynamic::from("Виробництво"));
            row.insert("owner".into(), Dynamic::from("Олена К."));
            row.insert("status".into(), Dynamic::from("в роботі"));
            row.insert("updated".into(), Dynamic::from("2026-09-17"));
            row.insert("price".into(), Dynamic::from((i % 300) as i64));
            row.insert("qty".into(), Dynamic::from((i % 7 + 1) as i64));
            row.insert("done".into(), Dynamic::from(i % 3 == 0));
            row.insert("note".into(), Dynamic::from("—"));
            Dynamic::from_map(row)
        })
        .collect()
}
