//! Розбір `multipart/form-data` — тіла форми із файлами.
//!
//! Тіло вже повністю в пам'яті, тож парсер синхронний і простий. Межу
//! (`boundary`) клієнт зобов'язаний обрати так, щоб вона не траплялась у даних
//! (RFC 7578), тому поділ за роздільником коректний і для двійкових файлів.

/// Одна частина форми: або текстове поле, або файл.
#[derive(Debug, Clone)]
pub struct Part {
    /// `name="..."` із `Content-Disposition`.
    pub name: String,
    /// `filename="..."`, якщо частина — файл.
    pub filename: Option<String>,
    pub content_type: Option<String>,
    pub data: Vec<u8>,
}

impl Part {
    /// Частина є файлом, якщо в неї є `filename`.
    pub fn is_file(&self) -> bool {
        self.filename.is_some()
    }
}

/// Дістати `boundary=...` із заголовка `Content-Type`.
pub fn boundary(content_type: &str) -> Option<String> {
    let marker = "boundary=";
    let start = content_type.find(marker)? + marker.len();
    let rest = &content_type[start..];
    // Межа може бути в лапках.
    let value = rest.trim_start_matches('"');
    let end = value.find(['"', ';']).unwrap_or(value.len());
    let value = value[..end].trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// Розібрати тіло на частини. Пошкоджене тіло дає порожній список, а не помилку:
/// сторінка сама вирішить, що робити з відсутнім файлом.
pub fn parse(body: &[u8], boundary: &str) -> Vec<Part> {
    let delimiter = format!("--{boundary}");
    let delimiter = delimiter.as_bytes();

    let mut parts = Vec::new();
    let mut segments = split_on(body, delimiter);

    // Перший сегмент — префікс перед першою межею (зазвичай порожній);
    // останній — хвіст після `--boundary--`. Обидва пропускаємо.
    for segment in segments.by_ref() {
        // Кінець форми: роздільник, за яким одразу `--`.
        if segment.starts_with(b"--") {
            break;
        }
        // Кожен сегмент починається з `\r\n` після межі й закінчується `\r\n`
        // перед наступною. Знімаємо обидва.
        let segment = strip_prefix(segment, b"\r\n");
        let segment = strip_suffix(segment, b"\r\n");
        if let Some(part) = parse_part(segment) {
            parts.push(part);
        }
    }
    parts
}

fn parse_part(segment: &[u8]) -> Option<Part> {
    // Заголовки відділені від тіла порожнім рядком.
    let split = find(segment, b"\r\n\r\n")?;
    let (head, rest) = segment.split_at(split);
    let data = &rest[4..];

    let head = String::from_utf8_lossy(head);
    let mut name = None;
    let mut filename = None;
    let mut content_type = None;

    for line in head.split("\r\n") {
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("content-disposition:") {
            name = extract(line, "name=");
            filename = extract(line, "filename=");
        } else if lower.starts_with("content-type:") {
            content_type = line.split_once(':').map(|(_, v)| v.trim().to_owned());
        }
    }

    Some(Part {
        name: name?,
        filename: filename.filter(|f| !f.is_empty()),
        content_type,
        data: data.to_vec(),
    })
}

/// Витягти значення параметра `key="..."` із рядка заголовка.
///
/// Саме параметра, а не підрядка: `line.find("name=")` у рядку
/// `filename="a.pdf"; name="doc"` знайшло б `name=` усередині `filename=`, і
/// поле назвалося б «a.pdf». Порядок параметрів RFC 7578 не фіксує.
fn extract(line: &str, key: &str) -> Option<String> {
    let mut rest = line.split_once(':').map(|(_, value)| value).unwrap_or(line);
    loop {
        rest = rest.trim_start_matches([' ', ';', '\t']);
        if rest.is_empty() {
            return None;
        }
        let (name, after) = match rest.find(['=', ';']) {
            Some(index) if rest.as_bytes()[index] == b'=' => (&rest[..index], &rest[index + 1..]),
            // Параметр без значення (`form-data`) — просто пропускаємо.
            Some(index) => {
                rest = &rest[index..];
                continue;
            }
            None => return None,
        };
        // Значення в лапках може містити `;` — читаємо до закривальної лапки.
        let (value, tail) = match after.strip_prefix('"') {
            Some(quoted) => {
                let end = quoted.find('"')?;
                (&quoted[..end], &quoted[end + 1..])
            }
            None => {
                let end = after.find(';').unwrap_or(after.len());
                (after[..end].trim(), &after[end..])
            }
        };
        if name.trim().eq_ignore_ascii_case(key.trim_end_matches('=')) {
            return Some(value.to_owned());
        }
        rest = tail;
    }
}

// ------------------------------------------------- дрібні байтові помічники

fn split_on<'a>(haystack: &'a [u8], needle: &[u8]) -> impl Iterator<Item = &'a [u8]> {
    let mut positions = Vec::new();
    let mut start = 0;
    while let Some(pos) = find(&haystack[start..], needle) {
        positions.push(start + pos);
        start += pos + needle.len();
    }
    let needle_len = needle.len();
    let mut bounds = Vec::new();
    let mut prev = 0;
    for &pos in &positions {
        bounds.push((prev, pos));
        prev = pos + needle_len;
    }
    bounds.push((prev, haystack.len()));
    bounds.into_iter().map(move |(a, b)| &haystack[a..b])
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn strip_prefix<'a>(data: &'a [u8], prefix: &[u8]) -> &'a [u8] {
    data.strip_prefix(prefix).unwrap_or(data)
}

fn strip_suffix<'a>(data: &'a [u8], suffix: &[u8]) -> &'a [u8] {
    data.strip_suffix(suffix).unwrap_or(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(boundary: &str) -> Vec<u8> {
        // Текстове поле + файл із двійковими байтами (зокрема 0x00).
        let mut b = Vec::new();
        let head = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"title\"\r\n\r\nПривіт\r\n\
             --{boundary}\r\nContent-Disposition: form-data; name=\"photo\"; filename=\"p.png\"\r\n\
             Content-Type: image/png\r\n\r\n"
        );
        b.extend_from_slice(head.as_bytes());
        b.extend_from_slice(&[0x00, 0x01, 0xFF, 0x0A]);
        b.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        b
    }

    #[test]
    fn boundary_is_extracted() {
        assert_eq!(
            boundary("multipart/form-data; boundary=abc123").as_deref(),
            Some("abc123")
        );
        assert_eq!(
            boundary("multipart/form-data; boundary=\"a b\"; charset=utf-8").as_deref(),
            Some("a b")
        );
        assert_eq!(boundary("application/json"), None);
    }

    #[test]
    fn text_and_file_parts_are_parsed() {
        let parts = parse(&body("BOUND"), "BOUND");
        assert_eq!(parts.len(), 2);

        let title = &parts[0];
        assert_eq!(title.name, "title");
        assert!(!title.is_file());
        assert_eq!(String::from_utf8_lossy(&title.data), "Привіт");

        let photo = &parts[1];
        assert_eq!(photo.name, "photo");
        assert!(photo.is_file());
        assert_eq!(photo.filename.as_deref(), Some("p.png"));
        assert_eq!(photo.content_type.as_deref(), Some("image/png"));
        // Двійкові дані цілі, включно з нульовим байтом.
        assert_eq!(photo.data, vec![0x00, 0x01, 0xFF, 0x0A]);
    }

    #[test]
    fn an_empty_file_input_has_no_filename() {
        // Форма з файловим полем, у якому нічого не вибрали: filename="".
        let boundary = "B";
        let raw = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"doc\"; filename=\"\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n\r\n--{boundary}--\r\n"
        );
        let parts = parse(raw.as_bytes(), boundary);
        assert_eq!(parts.len(), 1);
        assert!(!parts[0].is_file(), "порожнє поле файлу не є файлом");
    }

    #[test]
    fn garbage_body_yields_no_parts() {
        assert!(parse(b"not multipart at all", "BOUND").is_empty());
    }

    #[test]
    fn parameter_order_does_not_matter() {
        // `find("name=")` знаходив `name=` усередині `filename=`.
        let line = r#"Content-Disposition: form-data; filename="a.pdf"; name="doc""#;
        assert_eq!(extract(line, "name=").as_deref(), Some("doc"));
        assert_eq!(extract(line, "filename=").as_deref(), Some("a.pdf"));
    }

    #[test]
    fn quoted_values_may_hold_separators() {
        let line = r#"Content-Disposition: form-data; name="doc"; filename="звіт; квітень.pdf""#;
        assert_eq!(
            extract(line, "filename=").as_deref(),
            Some("звіт; квітень.pdf")
        );
        assert_eq!(extract(line, "name=").as_deref(), Some("doc"));
        // Без лапок і з іншим регістром імені параметра.
        let line = "content-disposition: form-data; NAME=plain";
        assert_eq!(extract(line, "name=").as_deref(), Some("plain"));
        assert_eq!(extract(line, "filename="), None);
    }
}
