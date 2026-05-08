use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MailSourceKind {
    Imap,
    ProtonCustom,
}

impl MailSourceKind {
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "imap" => Some(Self::Imap),
            "proton_custom" | "proton-custom" | "proton" => Some(Self::ProtonCustom),
            _ => None,
        }
    }
}

impl fmt::Display for MailSourceKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Imap => formatter.write_str("imap"),
            Self::ProtonCustom => formatter.write_str("proton_custom"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MessageRef {
    pub source: MailSourceKind,
    pub source_id: String,
    pub mailbox: Option<String>,
    pub stable_id: String,
}

impl MessageRef {
    #[must_use]
    pub fn new(
        source: MailSourceKind,
        source_id: impl Into<String>,
        mailbox: Option<String>,
        stable_id: impl Into<String>,
    ) -> Self {
        Self {
            source,
            source_id: source_id.into(),
            mailbox,
            stable_id: stable_id.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawEmail {
    pub bytes: Vec<u8>,
    pub source_ref: MessageRef,
}

impl RawEmail {
    #[must_use]
    pub fn new(bytes: Vec<u8>, source_ref: MessageRef) -> Self {
        Self { bytes, source_ref }
    }
}
