//! `paginate(total, per_page, page)` — уся арифметика сторінок одним викликом.
//!
//! Рецепт пагінації в кухарській книзі рахував `skip`, `pages`, межі й вікно
//! номерів руками. Це та сама формула в кожному застосунку, тому їй місце в
//! стандартній бібліотеці.
//!
//! ```rhai
//! let per = 20;
//! let total = db.count("orders");
//! let p = paginate(total, per, req.query_int("page") ?? 1);
//! let rows = db.find("orders", #{}, #{ sort: "id desc", limit: per, skip: p.skip });
//! ```
//!
//! У розмітці:
//!
//! ```html
//! <a @for={n in p.window} href={url("/orders", #{ page: n })}
//!    @class={#{"active": n == p.page}}>{{ n }}</a>
//! <p>Сторінка {{ p.page }} із {{ p.pages }}, показано {{ p.from }}–{{ p.to }} з {{ p.total }}</p>
//! ```

use rhai::{Array, Dynamic, Engine, Map, INT};

/// Скільки номерів показувати навколо поточного у `window`.
const WINDOW: i64 = 2;

/// Порахувати сторінку. `page` затискається в межі `1..=pages`, тому кривий
/// `?page=999` дає останню сторінку, а не порожнечу.
pub fn paginate(total: INT, per_page: INT, page: INT) -> Map {
    let total = total.max(0);
    let per_page = per_page.max(1);
    let pages = ((total + per_page - 1) / per_page).max(1);
    let page = page.clamp(1, pages);

    let skip = (page - 1) * per_page;
    let from = if total == 0 { 0 } else { skip + 1 };
    let to = (skip + per_page).min(total);

    // Вікно номерів навколо поточного, затиснуте в `1..=pages`.
    let start = (page - WINDOW).max(1);
    let end = (page + WINDOW).min(pages);
    let window: Array = (start..=end).map(Dynamic::from).collect();

    let mut map = Map::new();
    map.insert("page".into(), Dynamic::from(page));
    map.insert("pages".into(), Dynamic::from(pages));
    map.insert("per_page".into(), Dynamic::from(per_page));
    map.insert("total".into(), Dynamic::from(total));
    map.insert("skip".into(), Dynamic::from(skip));
    map.insert("from".into(), Dynamic::from(from));
    map.insert("to".into(), Dynamic::from(to));
    map.insert("has_prev".into(), Dynamic::from(page > 1));
    map.insert("has_next".into(), Dynamic::from(page < pages));
    // `prev`/`next` завжди валідні номери (на межах — та сама сторінка), тож їх
    // можна класти в посилання без перевірок.
    map.insert("prev".into(), Dynamic::from((page - 1).max(1)));
    map.insert("next".into(), Dynamic::from((page + 1).min(pages)));
    map.insert("first".into(), Dynamic::from(page == 1));
    map.insert("last".into(), Dynamic::from(page == pages));
    map.insert("window".into(), Dynamic::from_array(window));
    map
}

pub fn register_paginate(engine: &mut Engine) {
    engine
        .register_fn("paginate", |total: INT, per_page: INT, page: INT| {
            Dynamic::from_map(paginate(total, per_page, page))
        })
        // За замовчуванням 20 на сторінку — найчастіший випадок.
        .register_fn("paginate", |total: INT, page: INT| {
            Dynamic::from_map(paginate(total, 20, page))
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(map: &Map, key: &str) -> i64 {
        map[key].clone().cast::<i64>()
    }

    #[test]
    fn middle_page_has_correct_bounds() {
        // 45 записів по 20 → 3 сторінки; друга показує 21–40.
        let p = paginate(45, 20, 2);
        assert_eq!(get(&p, "pages"), 3);
        assert_eq!(get(&p, "skip"), 20);
        assert_eq!(get(&p, "from"), 21);
        assert_eq!(get(&p, "to"), 40);
        assert!(p["has_prev"].clone().cast::<bool>());
        assert!(p["has_next"].clone().cast::<bool>());
    }

    #[test]
    fn last_page_to_is_capped_at_total() {
        let p = paginate(45, 20, 3);
        assert_eq!(get(&p, "from"), 41);
        assert_eq!(get(&p, "to"), 45);
        assert!(!p["has_next"].clone().cast::<bool>());
        assert!(p["last"].clone().cast::<bool>());
    }

    #[test]
    fn out_of_range_page_is_clamped() {
        // ?page=999 → остання, ?page=0 → перша.
        assert_eq!(get(&paginate(45, 20, 999), "page"), 3);
        assert_eq!(get(&paginate(45, 20, 0), "page"), 1);
        assert_eq!(get(&paginate(45, 20, -5), "page"), 1);
    }

    #[test]
    fn empty_result_is_one_page_not_zero() {
        let p = paginate(0, 20, 1);
        assert_eq!(get(&p, "pages"), 1);
        assert_eq!(get(&p, "from"), 0);
        assert_eq!(get(&p, "to"), 0);
        assert!(p["first"].clone().cast::<bool>());
        assert!(p["last"].clone().cast::<bool>());
    }

    #[test]
    fn window_stays_within_bounds() {
        // Сторінка 1 із 10 → вікно 1..=3 (не йде в нуль і мінус).
        let p = paginate(200, 20, 1);
        let window: Vec<i64> = p["window"]
            .clone()
            .cast::<Array>()
            .into_iter()
            .map(|d| d.cast::<i64>())
            .collect();
        assert_eq!(window, vec![1, 2, 3]);

        // Остання сторінка → вікно не виходить за pages.
        let p = paginate(200, 20, 10);
        let window: Vec<i64> = p["window"]
            .clone()
            .cast::<Array>()
            .into_iter()
            .map(|d| d.cast::<i64>())
            .collect();
        assert_eq!(window, vec![8, 9, 10]);
    }

    #[test]
    fn per_page_zero_does_not_divide_by_zero() {
        let p = paginate(10, 0, 1);
        assert_eq!(get(&p, "per_page"), 1);
        assert_eq!(get(&p, "pages"), 10);
    }
}
