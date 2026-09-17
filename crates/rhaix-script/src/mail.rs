//! `mail.send(...)` — надсилання листа.
//!
//! Два режими, як у драйверів бази:
//!
//! - **dev / без feature `mail`** — лист друкується в лог, а не надсилається.
//!   Рецепт із поштою працює без жодного SMTP-сервера, і в консолі видно, що
//!   саме пішло б. Це режим за замовчуванням.
//! - **feature `mail` + секція `[mail]` у `rhaix.toml`** — справжнє надсилання
//!   через SMTP (`lettre`, синхронний транспорт).
//!
//! ```rhai
//! mail.send(#{
//!     to: "user@example.com",
//!     subject: "Вітаємо",
//!     text: "Дякуємо за реєстрацію.",
//!     html: "<p>Дякуємо за реєстрацію.</p>",   // необов'язково
//! });
//! ```
//!
//! `send` повертає `true`, якщо лист прийнято (у dev — завжди), і кидає помилку
//! з поясненням, якщо бракує поля `to` чи `subject` — це помилка автора, і краще
//! сказати одразу.

use std::sync::Arc;

use rhai::{Engine, EvalAltResult, Map};

/// Налаштування пошти з `[mail]`. Наповнює сервер.
#[derive(Debug, Clone, Default)]
pub struct MailConfig {
    /// Адреса відправника: `"Назва <noreply@host>"` або просто адреса.
    pub from: String,
    /// SMTP-хост. Якщо порожній — dev-режим (лог замість надсилання).
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
}

impl MailConfig {
    /// Чи налаштований справжній сервер.
    fn configured(&self) -> bool {
        !self.host.is_empty()
    }
}

/// `mail` у скрипті.
#[derive(Clone)]
pub struct Mail {
    config: Arc<MailConfig>,
}

impl Mail {
    pub fn new(config: MailConfig) -> Self {
        Self {
            config: Arc::new(config),
        }
    }
}

impl std::fmt::Debug for Mail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Mail")
    }
}

/// Розібраний лист — те, що прийшло з `send(#{...})`.
struct Letter {
    to: String,
    subject: String,
    text: String,
    html: Option<String>,
}

fn parse_letter(message: &Map) -> Result<Letter, Box<EvalAltResult>> {
    let field = |name: &str| -> String {
        message.get(name).map(super::display).unwrap_or_default()
    };
    let to = field("to");
    let subject = field("subject");
    if to.trim().is_empty() {
        return Err("mail.send: не вказано `to`".into());
    }
    if subject.trim().is_empty() {
        return Err("mail.send: не вказано `subject`".into());
    }
    let html = message.get("html").map(super::display).filter(|s| !s.is_empty());
    Ok(Letter {
        to,
        subject,
        text: field("text"),
        html,
    })
}

impl Mail {
    fn send(&self, message: Map) -> Result<bool, Box<EvalAltResult>> {
        let letter = parse_letter(&message)?;

        if !self.config.configured() {
            // Dev-режим: показуємо лист, а не надсилаємо.
            tracing::info!(
                "mail (dev): до `{}`, тема `{}`{}\n{}",
                letter.to,
                letter.subject,
                if letter.html.is_some() { " [+html]" } else { "" },
                letter.text
            );
            return Ok(true);
        }

        self.deliver(&letter)
    }

    #[cfg(feature = "mail")]
    fn deliver(&self, letter: &Letter) -> Result<bool, Box<EvalAltResult>> {
        use lettre::message::{header::ContentType, MultiPart, SinglePart};
        use lettre::transport::smtp::authentication::Credentials;
        use lettre::{Message, SmtpTransport, Transport};

        let from = self
            .config
            .from
            .parse()
            .map_err(|e| -> Box<EvalAltResult> { format!("mail: невірний `from`: {e}").into() })?;
        let to = letter
            .to
            .parse()
            .map_err(|e| -> Box<EvalAltResult> { format!("mail: невірний `to`: {e}").into() })?;

        let builder = Message::builder().from(from).to(to).subject(&letter.subject);
        let email = match &letter.html {
            Some(html) => builder.multipart(
                MultiPart::alternative()
                    .singlepart(SinglePart::plain(letter.text.clone()))
                    .singlepart(
                        SinglePart::builder()
                            .header(ContentType::TEXT_HTML)
                            .body(html.clone()),
                    ),
            ),
            None => builder
                .header(ContentType::TEXT_PLAIN)
                .body(letter.text.clone()),
        }
        .map_err(|e| -> Box<EvalAltResult> { format!("mail: не зібрати лист: {e}").into() })?;

        let mut transport = SmtpTransport::starttls_relay(&self.config.host)
            .map_err(|e| -> Box<EvalAltResult> { format!("mail: SMTP: {e}").into() })?
            .port(self.config.port);
        if !self.config.user.is_empty() {
            transport = transport.credentials(Credentials::new(
                self.config.user.clone(),
                self.config.password.clone(),
            ));
        }

        transport
            .build()
            .send(&email)
            .map(|_| true)
            .map_err(|e| -> Box<EvalAltResult> { format!("mail: не надіслано: {e}").into() })
    }

    #[cfg(not(feature = "mail"))]
    fn deliver(&self, _letter: &Letter) -> Result<bool, Box<EvalAltResult>> {
        // Секція [mail] задана, але бінарник зібрано без feature `mail`.
        Err("mail: надсилання не увімкнено в цій збірці; додайте feature `mail` \
             (у проді це робить `rhaix build` за секцією [mail])"
            .into())
    }
}

pub fn register_mail(engine: &mut Engine) {
    engine
        .register_type_with_name::<Mail>("Mail")
        .register_fn("send", |mail: &mut Mail, message: Map| mail.send(message));
}

#[cfg(test)]
mod tests {
    use super::*;
    use rhai::Dynamic;

    fn letter(pairs: &[(&str, &str)]) -> Map {
        pairs
            .iter()
            .map(|(k, v)| ((*k).into(), Dynamic::from((*v).to_owned())))
            .collect()
    }

    #[test]
    fn dev_mode_accepts_a_well_formed_letter() {
        // Без [mail] — dev-режим: лист приймається (логується), send → true.
        let mail = Mail::new(MailConfig::default());
        let ok = mail
            .send(letter(&[("to", "a@b.co"), ("subject", "Тема"), ("text", "текст")]))
            .expect("лист має бути прийнятий");
        assert!(ok);
    }

    #[test]
    fn missing_fields_are_reported() {
        let mail = Mail::new(MailConfig::default());
        let err = mail
            .send(letter(&[("subject", "без адресата")]))
            .unwrap_err();
        assert!(err.to_string().contains("to"), "{err}");

        let err = mail.send(letter(&[("to", "a@b.co")])).unwrap_err();
        assert!(err.to_string().contains("subject"), "{err}");
    }

    #[test]
    fn parse_letter_reads_html_when_present() {
        let l = parse_letter(&letter(&[
            ("to", "a@b.co"),
            ("subject", "s"),
            ("text", "t"),
            ("html", "<p>t</p>"),
        ]))
        .unwrap();
        assert_eq!(l.html.as_deref(), Some("<p>t</p>"));

        let plain = parse_letter(&letter(&[("to", "a@b.co"), ("subject", "s")])).unwrap();
        assert!(plain.html.is_none());
    }
}
