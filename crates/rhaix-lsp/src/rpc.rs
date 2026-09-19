//! Транспорт LSP: кадрування повідомлень і позиції.
//!
//! Протокол простий: заголовок `Content-Length`, порожній рядок, тіло JSON.
//! Тому власне читання/запис замість крейта — того самого розміру рішення, що
//! й розбір multipart у сервері.
//!
//! Найтонше місце тут — **позиції**. LSP рахує символи в UTF-16, а rhaix — у
//! байтах UTF-8. Для «Привіт» це різні числа, і саме тут мовні сервери
//! зазвичай промахуються на кирилиці.

use std::io::{BufRead, Write};

use serde_json::{json, Value};

/// Прочитати одне повідомлення зі stdin. `None` — потік закрито.
pub fn read_message(input: &mut impl BufRead) -> Option<Value> {
    let mut length: Option<usize> = None;

    loop {
        let mut line = String::new();
        if input.read_line(&mut line).ok()? == 0 {
            return None; // потік закрито
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break; // порожній рядок — далі тіло
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            length = value.trim().parse().ok();
        }
    }

    let length = length?;
    let mut body = vec![0u8; length];
    input.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

/// Надіслати повідомлення у stdout.
pub fn write_message(output: &mut impl Write, message: &Value) {
    let body = message.to_string();
    let _ = write!(output, "Content-Length: {}\r\n\r\n{}", body.len(), body);
    let _ = output.flush();
}

/// Відповідь на запит.
pub fn response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Сповіщення (без id): наприклад, діагностика.
pub fn notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

// ------------------------------------------------------------- позиції

/// Байтовий зсув у тексті → позиція LSP (рядок і символ у **UTF-16**).
pub fn offset_to_position(text: &str, offset: usize) -> (u32, u32) {
    let offset = offset.min(text.len());
    let mut line = 0u32;
    let mut line_start = 0usize;

    for (index, ch) in text.char_indices() {
        if index >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            line_start = index + 1;
        }
    }

    // Символи рядка до потрібного зсуву — у кодових одиницях UTF-16.
    let character = text[line_start..offset]
        .chars()
        .map(char::len_utf16)
        .sum::<usize>() as u32;
    (line, character)
}

/// Позиція LSP → байтовий зсув. Зворотна дія, потрібна для наведення й
/// доповнення: редактор каже рядок і символ, а нам треба місце в тексті.
pub fn position_to_offset(text: &str, line: u32, character: u32) -> usize {
    let mut current_line = 0u32;
    let mut line_start = 0usize;

    if line > 0 {
        for (index, ch) in text.char_indices() {
            if ch == '\n' {
                current_line += 1;
                if current_line == line {
                    line_start = index + 1;
                    break;
                }
            }
        }
        if current_line < line {
            return text.len(); // рядка немає — кінець тексту
        }
    }

    let rest = &text[line_start..];
    let mut utf16 = 0u32;
    for (index, ch) in rest.char_indices() {
        if utf16 >= character {
            return line_start + index;
        }
        utf16 += ch.len_utf16() as u32;
        if ch == '\n' {
            return line_start + index;
        }
    }
    text.len()
}

/// Діапазон LSP із двох байтових зсувів.
pub fn range(text: &str, start: usize, end: usize) -> Value {
    let (sl, sc) = offset_to_position(text, start);
    let (el, ec) = offset_to_position(text, end.max(start));
    json!({
        "start": { "line": sl, "character": sc },
        "end": { "line": el, "character": ec },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn framed_messages_round_trip() {
        let message = json!({ "jsonrpc": "2.0", "method": "initialize", "params": {} });
        let mut buffer = Vec::new();
        write_message(&mut buffer, &message);

        let text = String::from_utf8(buffer.clone()).unwrap();
        assert!(text.starts_with("Content-Length: "), "{text}");

        let mut cursor = Cursor::new(buffer);
        assert_eq!(read_message(&mut cursor), Some(message));
    }

    #[test]
    fn closed_stream_ends_the_loop() {
        let mut empty = Cursor::new(Vec::new());
        assert_eq!(read_message(&mut empty), None);
    }

    #[test]
    fn positions_count_utf16_not_bytes() {
        // «Привіт» — 12 байтів, але 6 символів UTF-16. Редактор чекає 6.
        let text = "<p>Привіт</p>";
        let offset = text.find("</p>").unwrap();
        let (line, character) = offset_to_position(text, offset);
        assert_eq!(line, 0);
        assert_eq!(character, 9, "3 символи `<p>` + 6 символів слова");
    }

    #[test]
    fn positions_handle_multiple_lines() {
        let text = "перший\nдругий\nтретій";
        let offset = text.find("третій").unwrap();
        assert_eq!(offset_to_position(text, offset), (2, 0));
    }

    #[test]
    fn position_to_offset_is_the_inverse() {
        let text = "---\nlet x = \"Привіт\";\n---\n<p>{{ x }}</p>";
        for probe in ["Привіт", "let", "{{ x }}", "<p>"] {
            let offset = text.find(probe).unwrap();
            let (line, character) = offset_to_position(text, offset);
            assert_eq!(
                position_to_offset(text, line, character),
                offset,
                "не зійшлося на `{probe}`"
            );
        }
    }

    #[test]
    fn out_of_range_positions_do_not_panic() {
        let text = "коротко";
        assert_eq!(position_to_offset(text, 99, 0), text.len());
        assert_eq!(position_to_offset(text, 0, 999), text.len());
        assert_eq!(offset_to_position(text, 9999).0, 0);
    }
}
