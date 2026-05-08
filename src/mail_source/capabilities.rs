#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailSourceCapabilities {
    pub can_mark_read: bool,
    pub has_stable_ids: bool,
    pub provides_raw_rfc822: bool,
}

impl MailSourceCapabilities {
    #[must_use]
    pub const fn imap() -> Self {
        Self {
            can_mark_read: true,
            has_stable_ids: true,
            provides_raw_rfc822: true,
        }
    }
}
