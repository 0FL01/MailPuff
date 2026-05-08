use std::fmt;

use time::OffsetDateTime;

pub mod parser;

#[derive(Clone, PartialEq, Eq)]
pub struct EmailSummary {
    pub subject: String,
    pub from_name: Option<String>,
    pub from_address: Option<String>,
    pub to_address: Option<String>,
    pub date: Option<OffsetDateTime>,
    pub html_body: Option<String>,
}

impl fmt::Debug for EmailSummary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EmailSummary")
            .field("subject", &self.subject)
            .field("from_name", &self.from_name)
            .field("from_address", &self.from_address)
            .field("to_address", &self.to_address)
            .field("date", &self.date)
            .field(
                "html_body",
                &self
                    .html_body
                    .as_ref()
                    .map(|body| RedactedBody { len: body.len() }),
            )
            .finish()
    }
}

struct RedactedBody {
    len: usize,
}

impl fmt::Debug for RedactedBody {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RedactedBody")
            .field("present", &true)
            .field("len", &self.len)
            .finish()
    }
}

impl EmailSummary {
    #[must_use]
    pub fn has_body(&self) -> bool {
        self.html_body
            .as_deref()
            .is_some_and(|body| !body.trim().is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_html_body() {
        let email = EmailSummary {
            subject: "subject".to_owned(),
            from_name: None,
            from_address: None,
            to_address: None,
            date: None,
            html_body: Some("<p>secret body</p>".to_owned()),
        };

        let debug = format!("{email:?}");

        assert!(debug.contains("RedactedBody"));
        assert!(!debug.contains("secret body"));
    }
}
