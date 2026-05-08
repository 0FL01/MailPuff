use time::OffsetDateTime;

pub mod parser;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailSummary {
    pub subject: String,
    pub from_name: Option<String>,
    pub from_address: Option<String>,
    pub to_address: Option<String>,
    pub date: Option<OffsetDateTime>,
    pub html_body: Option<String>,
}

impl EmailSummary {
    #[must_use]
    pub fn has_body(&self) -> bool {
        self.html_body
            .as_deref()
            .is_some_and(|body| !body.trim().is_empty())
    }
}
