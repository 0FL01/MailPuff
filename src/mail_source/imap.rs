use async_trait::async_trait;

use crate::{
    config::ImapConfig,
    error::{Error, Result},
    mail_source::{MailSource, MailSourceCapabilities, MessageRef, RawEmail},
};

#[derive(Debug, Clone)]
pub struct ImapSource {
    pub config: ImapConfig,
}

impl ImapSource {
    #[must_use]
    pub const fn new(config: ImapConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl MailSource for ImapSource {
    async fn list_unread(&self) -> Result<Vec<MessageRef>> {
        Err(Error::NotImplemented("IMAP mail source"))
    }

    async fn fetch(&self, _message: &MessageRef) -> Result<RawEmail> {
        Err(Error::NotImplemented("IMAP mail source"))
    }

    async fn mark_read(&self, _message: &MessageRef) -> Result<()> {
        Err(Error::NotImplemented("IMAP mail source"))
    }

    fn capabilities(&self) -> MailSourceCapabilities {
        MailSourceCapabilities::imap()
    }
}
