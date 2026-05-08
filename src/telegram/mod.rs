pub mod bot;
pub mod callbacks;

pub use bot::{SendEmailMessage, TelegramBot, TelegramError, TelegramMessageRef};
pub use callbacks::{CallbackStore, CreatedCallback, MarkCallbackPayload};
