//! Ворота M0: скільки коштує рендер таблиці 1000 × 10 виразів на Rhai.
//!
//! Перевіряємо не «швидкість інтерпретатора» взагалі, а два конкретні рішення,
//! від яких залежить архітектура M1-M2 (див. RISKS.md 2.2):
//!
//! 1. чи можна передавати дані в scope без глибокого клонування (shared-значення);
//! 2. чи варто мати fast-path для голого `{{ row.field }}` замість `eval_ast`.
//!
//! Запуск: `cargo run --release -p rhaix-script --bin rhaix-bench`

use std::time::{Duration, Instant};

use rhai::{Dynamic, Engine, Map, Scope, AST};
use rhaix_script::{compile_expression, display, engine, write_display, Limits};

const ROWS: usize = 1000;
const BIG_ROWS: usize = 5000;
const GATE_MS: f64 = 5.0;

/// Десять виразів на рядок — приблизно те, що дає реальна таблиця:
/// шість простих звернень до поля і чотири справжні обчислення.
const EXPRS: [&str; 10] = [
    "row.id",
    "row.title",
    "row.dept",
    "row.owner",
    "row.status",
    "row.updated",
    "row.price * row.qty",
    "if row.done { \"✔\" } else { \"…\" }",
    "row.title + \" (\" + row.dept + \")\"",
    "if row.price > 100 { \"дорого\" } else { \"норм\" }",
];

fn main() {
    let iters: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(20);

    let engine = engine(Limits::default());
    let asts: Vec<AST> = EXPRS
        .iter()
        .map(|src| compile_expression(&engine, src).expect("вираз з бенчмарку має компілюватись"))
        .collect();
    let rows = make_rows(ROWS);

    println!("rhaix — ворота продуктивності M0");
    println!(
        "{ROWS} рядків × {} виразів, {iters} прогонів\n",
        EXPRS.len()
    );

    let baseline = measure(iters, || render_rust(&rows));
    let naive = measure(iters, || render_naive(&engine, &asts, &rows));
    let shared = measure(iters, || render_shared(&engine, &asts, &rows));
    let fast_shared = measure(iters, || render_fast_path(&engine, &asts, &rows));
    let chosen = measure(iters, || render_chosen(&engine, &asts, &rows));

    println!("{:<36} {:>10} {:>10}", "стратегія", "медіана", "краще");
    report("Rust без Rhai (підлога)", &baseline);
    report("A. eval усього + клон рядка", &naive);
    report("B. eval усього + shared-рядок", &shared);
    report("C. fast-path + shared-рядок", &fast_shared);
    report("D. fast-path + звичайний рядок", &chosen);

    println!();
    println!("з чого складається D:");
    let only_fields = measure(iters, || render_fields_only(&rows));
    let only_exprs = measure(iters, || render_exprs_only(&engine, &asts, &rows));
    report("   6 звернень до поля (без Rhai)", &only_fields);
    report("   4 справжні вирази (через Rhai)", &only_exprs);

    println!();
    props_benchmark(iters);

    println!();
    let chosen_ms = median_ms(&chosen);
    if chosen_ms < GATE_MS {
        println!("PASS: обрана стратегія D — {chosen_ms:.2} мс < {GATE_MS:.0} мс");
    } else {
        println!("FAIL: обрана стратегія D — {chosen_ms:.2} мс ≥ {GATE_MS:.0} мс");
        println!("      далі з цим числом M1 починати не можна, спершу профілювання");
    }
}

/// D. Те, що піде в M1: рядок у scope — звичайним значенням (не shared, бо
///    блокування на кожному зверненні дорожче за клон), голі `row.field`
///    читаються прямо з мапи повз Rhai, вивід пишеться в буфер без алокацій.
fn render_chosen(engine: &Engine, asts: &[AST], rows: &[Map]) -> usize {
    let plan: Vec<Option<&str>> = EXPRS.iter().map(|src| field_access(src)).collect();
    let mut out = String::with_capacity(rows.len() * 160);
    let mut scope = Scope::new();

    for row in rows {
        scope.push_dynamic("row", Dynamic::from_map(row.clone()));
        for (ast, field) in asts.iter().zip(&plan) {
            match field {
                Some(name) => {
                    if let Some(value) = row.get(*name) {
                        write_display(&mut out, value);
                    }
                }
                None => {
                    let value = engine
                        .eval_ast_with_scope::<Dynamic>(&mut scope, ast)
                        .expect("вираз має обчислюватись");
                    write_display(&mut out, &value);
                }
            }
        }
        scope.rewind(0);
    }
    out.len()
}

/// Лише шість звернень до поля — скільки коштує «дешева» частина таблиці.
fn render_fields_only(rows: &[Map]) -> usize {
    let mut out = String::with_capacity(rows.len() * 120);
    for row in rows {
        for name in ["id", "title", "dept", "owner", "status", "updated"] {
            if let Some(value) = row.get(name) {
                write_display(&mut out, value);
            }
        }
    }
    out.len()
}

/// Лише чотири справжні вирази — скільки коштує сам Rhai.
fn render_exprs_only(engine: &Engine, asts: &[AST], rows: &[Map]) -> usize {
    let mut out = String::with_capacity(rows.len() * 60);
    let mut scope = Scope::new();
    for row in rows {
        scope.push_dynamic("row", Dynamic::from_map(row.clone()));
        for ast in &asts[6..] {
            let value = engine
                .eval_ast_with_scope::<Dynamic>(&mut scope, ast)
                .expect("вираз має обчислюватись");
            write_display(&mut out, &value);
        }
        scope.rewind(0);
    }
    out.len()
}

// ---------------------------------------------------------------- стратегії

/// Підлога: те саме форматування без скриптової мови взагалі.
fn render_rust(rows: &[Map]) -> usize {
    let mut out = String::with_capacity(rows.len() * 160);
    for row in rows {
        out.push_str("<tr>");
        for key in ["id", "title", "dept", "owner", "status", "updated"] {
            out.push_str("<td>");
            out.push_str(&display(&row[key]));
            out.push_str("</td>");
        }
        let total = row["price"].as_int().unwrap_or(0) * row["qty"].as_int().unwrap_or(0);
        out.push_str("<td>");
        out.push_str(&total.to_string());
        out.push_str("</td>");
        out.push_str(if row["done"].as_bool().unwrap_or(false) {
            "<td>✔</td>"
        } else {
            "<td>…</td>"
        });
        out.push_str("</tr>");
    }
    out.len()
}

/// A. Найпростіше, що працює: на кожен рядок — свіжа копія мапи в scope.
fn render_naive(engine: &Engine, asts: &[AST], rows: &[Map]) -> usize {
    let mut out = String::with_capacity(rows.len() * 160);
    let mut scope = Scope::new();
    for row in rows {
        scope.push_dynamic("row", Dynamic::from_map(row.clone()));
        for ast in asts {
            let value = engine
                .eval_ast_with_scope::<Dynamic>(&mut scope, ast)
                .expect("вираз має обчислюватись");
            out.push_str(&display(&value));
        }
        scope.rewind(0);
    }
    out.len()
}

/// B. Те саме, але рядок загорнутий у shared-значення: у scope потрапляє
///    дешевий Arc-клон замість глибокої копії мапи.
fn render_shared(engine: &Engine, asts: &[AST], rows: &[Map]) -> usize {
    let shared: Vec<Dynamic> = rows
        .iter()
        .map(|row| Dynamic::from_map(row.clone()).into_shared())
        .collect();
    let mut out = String::with_capacity(rows.len() * 160);
    let mut scope = Scope::new();
    for row in &shared {
        scope.push_dynamic("row", row.clone());
        for ast in asts {
            let value = engine
                .eval_ast_with_scope::<Dynamic>(&mut scope, ast)
                .expect("вираз має обчислюватись");
            out.push_str(&display(&value));
        }
        scope.rewind(0);
    }
    out.len()
}

/// C. Обрана стратегія: голе `row.field` читається прямо з мапи, через Rhai
///    йдуть лише справжні вирази.
fn render_fast_path(engine: &Engine, asts: &[AST], rows: &[Map]) -> usize {
    // Те, що в M1 робитиме компілятор шаблону: розпізнати тривіальний доступ
    // до поля й не тримати для нього AST узагалі.
    let plan: Vec<Option<&str>> = EXPRS.iter().map(|src| field_access(src)).collect();

    let shared: Vec<Dynamic> = rows
        .iter()
        .map(|row| Dynamic::from_map(row.clone()).into_shared())
        .collect();
    let mut out = String::with_capacity(rows.len() * 160);
    let mut scope = Scope::new();

    for row in &shared {
        scope.push_dynamic("row", row.clone());
        for (ast, field) in asts.iter().zip(&plan) {
            match field {
                Some(name) => {
                    let guard = row.read_lock::<Map>().expect("рядок — це мапа");
                    if let Some(value) = guard.get(*name) {
                        out.push_str(&display(value));
                    }
                }
                None => {
                    let value = engine
                        .eval_ast_with_scope::<Dynamic>(&mut scope, ast)
                        .expect("вираз має обчислюватись");
                    out.push_str(&display(&value));
                }
            }
        }
        scope.rewind(0);
    }
    out.len()
}

/// `row.title` → `Some("title")`; будь-що складніше → `None`.
fn field_access(expr: &str) -> Option<&str> {
    let rest = expr.trim().strip_prefix("row.")?;
    rest.chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        .then_some(rest)
}

// ------------------------------------------------- передача props у компонент

/// Скільки коштує передати колекцію в компонент: копією чи shared-значенням.
fn props_benchmark(iters: usize) {
    let rows = make_rows(BIG_ROWS);
    let array: Vec<Dynamic> = rows.iter().cloned().map(Dynamic::from_map).collect();
    let as_dynamic = Dynamic::from_array(array);
    let as_shared = as_dynamic.clone().into_shared();

    println!("передача колекції з {BIG_ROWS} рядків у props (× 10 вкладених компонентів)");
    let deep = measure(iters, || {
        let mut sink = 0usize;
        for _ in 0..10 {
            let copy = as_dynamic.clone();
            sink += copy.read_lock::<rhai::Array>().map_or(0, |a| a.len());
        }
        sink
    });
    let cheap = measure(iters, || {
        let mut sink = 0usize;
        for _ in 0..10 {
            let copy = as_shared.clone();
            sink += copy.read_lock::<rhai::Array>().map_or(0, |a| a.len());
        }
        sink
    });
    report("D. глибокий клон", &deep);
    report("E. shared (Arc-клон)", &cheap);

    let ratio = median_ms(&deep) / median_ms(&cheap).max(f64::MIN_POSITIVE);
    println!("   shared дешевший у {ratio:.0}×");
}

// ---------------------------------------------------------------- інструменти

fn make_rows(n: usize) -> Vec<Map> {
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
            row
        })
        .collect()
}

fn measure<T>(iters: usize, mut f: impl FnMut() -> T) -> Vec<Duration> {
    // прогрів, щоб не міряти перший холодний прогін
    let _ = f();
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let started = Instant::now();
        let out = f();
        samples.push(started.elapsed());
        std::hint::black_box(out);
    }
    samples.sort();
    samples
}

fn median_ms(samples: &[Duration]) -> f64 {
    samples[samples.len() / 2].as_secs_f64() * 1000.0
}

fn report(label: &str, samples: &[Duration]) {
    let best = samples[0].as_secs_f64() * 1000.0;
    let median = median_ms(samples);
    println!("{label:<36} {median:>8.2} мс {best:>8.2} мс");
}
