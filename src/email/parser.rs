use html_escape::encode_text;
use mail_parser::{Addr, Address, DateTime, MessageParser, PartType};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::email::EmailSummary;

const NO_SUBJECT: &str = "(no subject)";
const UNKNOWN_SENDER_NAME: &str = "Unknown sender";
const UNKNOWN_SENDER_ADDRESS: &str = "unknown@unknown";

pub fn parse_email_summary(raw: &[u8]) -> Result<EmailSummary, EmailParseError> {
    let message = MessageParser::default()
        .parse(raw)
        .ok_or(EmailParseError::ParseFailed)?;

    let from = message.from().and_then(first_addr).map(OwnedAddress::from);
    let to_address = message.to().and_then(first_addr).and_then(address_value);
    let html_body = html_body(&message).or_else(|| text_body_as_html(&message));

    Ok(EmailSummary {
        subject: clean_header_value(message.subject()).unwrap_or_else(|| NO_SUBJECT.to_owned()),
        from_name: Some(
            from.as_ref()
                .and_then(|address| address.name.clone())
                .unwrap_or_else(|| UNKNOWN_SENDER_NAME.to_owned()),
        ),
        from_address: Some(
            from.as_ref()
                .and_then(|address| address.address.clone())
                .unwrap_or_else(|| UNKNOWN_SENDER_ADDRESS.to_owned()),
        ),
        to_address,
        date: message.date().and_then(offset_date_time),
        html_body,
    })
}

fn html_body(message: &mail_parser::Message<'_>) -> Option<String> {
    let part = message.html_part(0)?;

    match &part.body {
        PartType::Html(html) => non_empty_string(html.as_ref()),
        _ => None,
    }
}

fn text_body_as_html(message: &mail_parser::Message<'_>) -> Option<String> {
    let part = message.text_part(0)?;

    match &part.body {
        PartType::Text(text) => {
            let text = text.as_ref();
            (!text.trim().is_empty()).then(|| {
                format!(
                    "<pre style=\"white-space:pre-wrap;word-wrap:break-word;\">{}</pre>",
                    encode_text(text)
                )
            })
        }
        _ => None,
    }
}

fn first_addr<'address, 'message>(
    address: &'address Address<'message>,
) -> Option<&'address Addr<'message>> {
    address.first()
}

fn clean_header_value(value: Option<&str>) -> Option<String> {
    value.and_then(non_empty_string)
}

fn non_empty_string(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn address_value(address: &Addr<'_>) -> Option<String> {
    address.address.as_deref().and_then(non_empty_string)
}

fn offset_date_time(date: &DateTime) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(&date.to_rfc3339(), &Rfc3339).ok()
}

#[derive(Debug, Clone)]
struct OwnedAddress {
    name: Option<String>,
    address: Option<String>,
}

impl From<&Addr<'_>> for OwnedAddress {
    fn from(address: &Addr<'_>) -> Self {
        Self {
            name: address.name.as_deref().and_then(non_empty_string),
            address: address_value(address),
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EmailParseError {
    #[error("failed to parse RFC822 email")]
    ParseFailed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_html_body() -> Result<(), EmailParseError> {
        let raw = concat!(
            "From: Alice Example <alice@example.com>\r\n",
            "To: Bob <bob@example.com>\r\n",
            "Subject: Hello =?utf-8?B?8J+Yig==?=\r\n",
            "Date: Tue, 07 May 2024 12:34:56 +0300\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: multipart/alternative; boundary=\"b\"\r\n",
            "\r\n",
            "--b\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n",
            "\r\n",
            "Plain body\r\n",
            "--b\r\n",
            "Content-Type: text/html; charset=utf-8\r\n",
            "\r\n",
            "<p><strong>HTML body</strong></p>\r\n",
            "--b--\r\n",
        )
        .as_bytes();

        let summary = parse_email_summary(raw)?;

        assert_eq!(summary.subject, "Hello 😊");
        assert_eq!(summary.from_name.as_deref(), Some("Alice Example"));
        assert_eq!(summary.from_address.as_deref(), Some("alice@example.com"));
        assert_eq!(summary.to_address.as_deref(), Some("bob@example.com"));
        assert_eq!(
            summary.html_body.as_deref(),
            Some("<p><strong>HTML body</strong></p>")
        );
        assert!(summary.date.is_some());

        Ok(())
    }

    #[test]
    fn escapes_text_plain_fallback() -> Result<(), EmailParseError> {
        let raw = concat!(
            "From: sender@example.com\r\n",
            "Subject: Text only\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n",
            "\r\n",
            "Hello <script>alert(1)</script>\r\n",
        )
        .as_bytes();

        let summary = parse_email_summary(raw)?;
        let html = summary.html_body.as_deref().unwrap_or_default();

        assert!(html.starts_with("<pre style=\"white-space:pre-wrap;word-wrap:break-word;\">"));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!html.contains("<script>"));

        Ok(())
    }

    #[test]
    fn no_body_returns_empty_body() -> Result<(), EmailParseError> {
        let raw = concat!("From: sender@example.com\r\n", "Subject: Empty\r\n", "\r\n").as_bytes();

        let summary = parse_email_summary(raw)?;

        assert_eq!(summary.html_body, None);
        assert!(!summary.has_body());

        Ok(())
    }

    #[test]
    fn applies_sender_and_subject_fallbacks() -> Result<(), EmailParseError> {
        let raw = concat!(
            "Content-Type: text/plain; charset=utf-8\r\n",
            "\r\n",
            "Body\r\n",
        )
        .as_bytes();

        let summary = parse_email_summary(raw)?;

        assert_eq!(summary.subject, NO_SUBJECT);
        assert_eq!(summary.from_name.as_deref(), Some(UNKNOWN_SENDER_NAME));
        assert_eq!(
            summary.from_address.as_deref(),
            Some(UNKNOWN_SENDER_ADDRESS)
        );

        Ok(())
    }
}
