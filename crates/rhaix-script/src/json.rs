//! Міст між JSON і значеннями Rhai.
//!
//! Потрібен трьом місцям одразу: сесія їде в cookie як JSON, `http` розбирає
//! відповідь API, а `json_decode()` доступний просто так. Тому конвертація
//! живе окремо, а не всередині котрогось із них.

use rhai::{Array, Dynamic, Map};
use serde_json::{Number, Value};

/// JSON → значення Rhai.
///
/// Числа: ціле лишається цілим (`id` із API має порівнюватись із `id` із бази),
/// решта стає `f64`.
pub fn to_dynamic(value: &Value) -> Dynamic {
    match value {
        Value::Null => Dynamic::UNIT,
        Value::Bool(flag) => Dynamic::from(*flag),
        Value::Number(number) => number_to_dynamic(number),
        Value::String(text) => Dynamic::from(text.clone()),
        Value::Array(items) => {
            let array: Array = items.iter().map(to_dynamic).collect();
            Dynamic::from_array(array)
        }
        Value::Object(fields) => {
            let mut map = Map::new();
            for (key, item) in fields {
                map.insert(key.as_str().into(), to_dynamic(item));
            }
            Dynamic::from_map(map)
        }
    }
}

fn number_to_dynamic(number: &Number) -> Dynamic {
    if let Some(int) = number.as_i64() {
        return Dynamic::from(int);
    }
    match number.as_f64() {
        Some(float) => Dynamic::from(float),
        None => Dynamic::UNIT,
    }
}

/// Значення Rhai → JSON. Те, що не серіалізується, стає `null`.
pub fn from_dynamic(value: &Dynamic) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// Розібрати текст; `None` — якщо це не JSON.
pub fn parse(text: &str) -> Option<Dynamic> {
    serde_json::from_str::<Value>(text).ok().map(|v| to_dynamic(&v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_stay_integers() {
        let value = parse(r#"{"id": 7, "price": 1.5, "ok": true, "tags": ["a"], "none": null}"#)
            .expect("це JSON");
        let map = value.cast::<Map>();
        assert_eq!(map["id"].clone().cast::<i64>(), 7);
        assert_eq!(map["price"].clone().cast::<f64>(), 1.5);
        assert!(map["ok"].clone().cast::<bool>());
        assert_eq!(map["tags"].clone().cast::<Array>().len(), 1);
        assert!(map["none"].is_unit());
    }

    #[test]
    fn broken_json_is_not_an_error() {
        assert!(parse("<html>").is_none());
    }

    #[test]
    fn round_trip_keeps_the_shape() {
        let value = parse(r#"{"a":[1,2],"b":{"c":"д"}}"#).expect("це JSON");
        assert_eq!(from_dynamic(&value).to_string(), r#"{"a":[1,2],"b":{"c":"д"}}"#);
    }
}
