//! `live` — сповістити відкриті сторінки, що щось змінилось.
//!
//! ```rhai
//! db.insert("orders", order);
//! live.send("orders");                  // усім, хто слухає `live:orders`
//! live.send("orders", #{ id: id });     // з даними для власного JS
//! ```
//!
//! ```html
//! <tbody hx-get="/orders/rows" hx-trigger="live:orders from:body">…</tbody>
//! ```
//!
//! Сервер надсилає не HTML, а лише сигнал «тема змінилась». Кожна сторінка
//! перезапитує свої дані сама — зі своєю сесією й своїми правами. Тож через
//! канал не може «протекти» те, чого цей користувач бачити не мав би, і
//! серверу не треба рендерити сторінку окремо для кожного підписника.
//!
//! Крейт про транспорт не знає: сервер дає функцію публікації, а як вона
//! доставляє повідомлення (SSE, канал tokio) — його справа.

use std::sync::Arc;

use rhai::{Dynamic, Engine, EvalAltResult};

/// Куди віддати `(тема, detail у JSON)`.
pub type Publish = Arc<dyn Fn(&str, &str) + Send + Sync>;

/// `live` у скрипті.
#[derive(Clone, Default)]
pub struct Live(Option<Publish>);

impl Live {
    pub fn new(publish: Publish) -> Self {
        Self(Some(publish))
    }

    /// Без транспорту: `rhaix check`, тести. `send` просто нічого не робить.
    pub fn disabled() -> Self {
        Self(None)
    }
}

impl std::fmt::Debug for Live {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "Live"
        } else {
            "Live(вимкнено)"
        })
    }
}

/// Тема стає ім'ям DOM-події (`live:orders`) і параметром адреси, тож лише
/// прості символи: без пробілів, двокрапок і всього, що ламає `hx-trigger`.
fn valid_topic(topic: &str) -> bool {
    !topic.is_empty()
        && topic.len() <= 64
        && topic
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn send(live: &Live, topic: &str, detail: &Dynamic) -> Result<(), Box<EvalAltResult>> {
    if !valid_topic(topic) {
        return Err(format!(
            "live.send(): тема `{topic}` — лише латиниця, цифри, `_`, `-`, `.` (до 64 символів)"
        )
        .into());
    }
    if let Some(publish) = &live.0 {
        let json = if detail.is_unit() {
            "{}".to_owned()
        } else {
            crate::json_encode(detail)
        };
        publish(topic, &json);
    }
    Ok(())
}

pub fn register_live(engine: &mut Engine) {
    engine
        .register_type_with_name::<Live>("Live")
        .register_fn("send", |live: &mut Live, topic: &str| {
            send(live, topic, &Dynamic::UNIT)
        })
        .register_fn("send", |live: &mut Live, topic: &str, detail: Dynamic| {
            send(live, topic, &detail)
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{engine, Limits};
    use rhai::Scope;
    use std::sync::Mutex;

    #[test]
    fn send_publishes_the_topic_and_its_detail_as_json() {
        let sent: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
        let sink = sent.clone();
        let live = Live::new(Arc::new(move |topic: &str, detail: &str| {
            sink.lock()
                .unwrap()
                .push((topic.to_owned(), detail.to_owned()));
        }));
        let engine = engine(Limits::default());
        let mut scope = Scope::new();
        scope.push("live", live);
        engine
            .run_with_scope(
                &mut scope,
                r#"live.send("orders"); live.send("orders.7", #{ id: 7 });"#,
            )
            .unwrap();
        let sent = sent.lock().unwrap();
        assert_eq!(sent[0], ("orders".to_owned(), "{}".to_owned()));
        assert_eq!(sent[1], ("orders.7".to_owned(), r#"{"id":7}"#.to_owned()));
    }

    #[test]
    fn a_topic_that_would_break_hx_trigger_is_refused() {
        let engine = engine(Limits::default());
        let mut scope = Scope::new();
        scope.push("live", Live::disabled());
        for bad in ["", "замовлення", "a b", "a:b", "x,y"] {
            let err = engine
                .run_with_scope(&mut scope, &format!("live.send({bad:?});"))
                .unwrap_err();
            assert!(err.to_string().contains("тема"), "{bad}: {err}");
        }
        engine
            .run_with_scope(&mut scope, r#"live.send("ok_topic-1.2");"#)
            .unwrap();
    }
}
