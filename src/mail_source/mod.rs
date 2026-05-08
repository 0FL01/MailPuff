use async_trait::async_trait;

use crate::error::Result;

pub mod capabilities;
pub mod imap;
pub mod model;

pub use capabilities::MailSourceCapabilities;
pub use model::{MailSourceKind, MessageRef, RawEmail};

#[async_trait]
pub trait MailSource: Send + Sync {
    async fn list_unread(&self) -> Result<Vec<MessageRef>>;

    async fn fetch(&self, message: &MessageRef) -> Result<RawEmail>;

    async fn mark_read(&self, message: &MessageRef) -> Result<()>;

    fn capabilities(&self) -> MailSourceCapabilities;
}
