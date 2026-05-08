use std::fmt;

use async_trait::async_trait;
use html_escape::encode_text;
use secrecy::ExposeSecret;
use teloxide::{
    payloads::{AnswerCallbackQuerySetters, EditMessageReplyMarkupSetters, SendMessageSetters},
    prelude::Requester,
    types::{
        CallbackQueryId, ChatId, InlineKeyboardButton, InlineKeyboardMarkup, LinkPreviewOptions,
        MessageId, ParseMode,
    },
};
use thiserror::Error;
use url::Url;

use crate::{
    config::TelegramConfig, email::EmailSummary, telegram::callbacks::CallbackTelegramApi,
};

#[derive(Clone)]
pub struct TelegramBot {
    bot: teloxide::Bot,
    chat_id: ChatId,
}

impl TelegramBot {
    #[must_use]
    pub fn new(config: &TelegramConfig) -> Self {
        Self {
            bot: teloxide::Bot::new(config.token.expose_secret().to_owned()),
            chat_id: ChatId(config.chat_id),
        }
    }

    pub const fn raw(&self) -> &teloxide::Bot {
        &self.bot
    }

    pub async fn send_email_message(
        &self,
        request: SendEmailMessage<'_>,
    ) -> Result<TelegramMessageRef, TelegramError> {
        let sent = self
            .bot
            .send_message(self.chat_id, format_email_message(request.email))
            .parse_mode(ParseMode::Html)
            .link_preview_options(disabled_link_preview())
            .reply_markup(email_keyboard(
                request.viewer_url,
                request.mark_callback_data,
            ))
            .await?;

        Ok(TelegramMessageRef::new(sent.chat.id.0, sent.id.0))
    }

    pub async fn edit_open_only_keyboard(
        &self,
        message: TelegramMessageRef,
        viewer_url: Url,
    ) -> Result<(), TelegramError> {
        self.bot
            .edit_message_reply_markup(ChatId(message.chat_id), MessageId(message.message_id))
            .reply_markup(open_only_keyboard(viewer_url))
            .await?;

        Ok(())
    }

    pub async fn answer_callback(
        &self,
        callback_id: &str,
        text: &str,
    ) -> Result<(), TelegramError> {
        self.bot
            .answer_callback_query(CallbackQueryId(callback_id.to_owned()))
            .text(text.to_owned())
            .show_alert(false)
            .cache_time(0)
            .await?;

        Ok(())
    }
}

impl fmt::Debug for TelegramBot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TelegramBot")
            .field("token", &"[REDACTED]")
            .field("chat_id", &self.chat_id.0)
            .finish()
    }
}

#[async_trait]
impl CallbackTelegramApi for TelegramBot {
    async fn answer_callback(&self, callback_id: &str, text: &str) -> Result<(), TelegramError> {
        TelegramBot::answer_callback(self, callback_id, text).await
    }

    async fn edit_open_only_keyboard(
        &self,
        message: TelegramMessageRef,
        viewer_url: Url,
    ) -> Result<(), TelegramError> {
        TelegramBot::edit_open_only_keyboard(self, message, viewer_url).await
    }
}

#[derive(Debug, Clone)]
pub struct SendEmailMessage<'a> {
    pub email: &'a EmailSummary,
    pub viewer_url: Url,
    pub mark_callback_data: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TelegramMessageRef {
    pub chat_id: i64,
    pub message_id: i32,
}

impl TelegramMessageRef {
    #[must_use]
    pub const fn new(chat_id: i64, message_id: i32) -> Self {
        Self {
            chat_id,
            message_id,
        }
    }
}

#[derive(Debug, Error)]
pub enum TelegramError {
    #[error("Telegram API request failed: {0}")]
    Request(#[from] teloxide::RequestError),
}

#[must_use]
pub fn format_email_message(email: &EmailSummary) -> String {
    let subject = encode_telegram_html(&email.subject);
    let from_name = encode_telegram_html(
        email
            .from_name
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("Unknown sender"),
    );
    let from_address = encode_telegram_html(
        email
            .from_address
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("unknown@unknown"),
    );

    format!(
        "{subject}\n{from_name}\n\nA new email has arrived from this address: {from_address}\n\n🌐 A secret HTML page has been created for it, where you can preview the message by following the link below 👇"
    )
}

#[must_use]
pub fn email_keyboard(viewer_url: Url, mark_callback_data: String) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::url("Open html", viewer_url),
        InlineKeyboardButton::callback("Mark as read", mark_callback_data),
    ]])
}

#[must_use]
pub fn open_only_keyboard(viewer_url: Url) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::url(
        "Open html",
        viewer_url,
    )]])
}

fn encode_telegram_html(value: &str) -> String {
    encode_text(value).to_string()
}

fn disabled_link_preview() -> LinkPreviewOptions {
    LinkPreviewOptions {
        is_disabled: true,
        url: None,
        prefer_small_media: false,
        prefer_large_media: false,
        show_above_text: false,
    }
}

#[cfg(test)]
mod tests {
    use teloxide::types::InlineKeyboardButtonKind;

    use super::*;

    fn email_summary() -> EmailSummary {
        EmailSummary {
            subject: "<hello & bye>".to_owned(),
            from_name: Some("Alice <Admin>".to_owned()),
            from_address: Some("alice&ops@example.com".to_owned()),
            to_address: None,
            date: None,
            html_body: Some("<p>Hello</p>".to_owned()),
        }
    }

    #[test]
    fn formats_message_with_html_escaped_fields() {
        let text = format_email_message(&email_summary());

        assert!(text.contains("&lt;hello &amp; bye&gt;"));
        assert!(text.contains("Alice &lt;Admin&gt;"));
        assert!(text.contains("alice&amp;ops@example.com"));
        assert!(!text.contains("<hello"));
    }

    #[test]
    fn formats_message_with_sender_fallbacks() {
        let email = EmailSummary {
            from_name: Some(String::new()),
            from_address: None,
            ..email_summary()
        };

        let text = format_email_message(&email);

        assert!(text.contains("Unknown sender"));
        assert!(text.contains("unknown@unknown"));
    }

    #[test]
    fn builds_email_keyboard_with_open_and_mark_buttons() -> Result<(), url::ParseError> {
        let viewer_url = Url::parse("https://example.com/view?id=1&token=secret")?;
        let keyboard = email_keyboard(viewer_url.clone(), "mark:abc".to_owned());

        assert_eq!(keyboard.inline_keyboard.len(), 1);
        assert_eq!(keyboard.inline_keyboard[0].len(), 2);
        assert_eq!(keyboard.inline_keyboard[0][0].text, "Open html");
        assert_eq!(keyboard.inline_keyboard[0][1].text, "Mark as read");
        assert_eq!(
            keyboard.inline_keyboard[0][0].kind,
            InlineKeyboardButtonKind::Url(viewer_url)
        );
        assert_eq!(
            keyboard.inline_keyboard[0][1].kind,
            InlineKeyboardButtonKind::CallbackData("mark:abc".to_owned())
        );

        Ok(())
    }

    #[test]
    fn builds_open_only_keyboard() -> Result<(), url::ParseError> {
        let viewer_url = Url::parse("https://example.com/view")?;
        let keyboard = open_only_keyboard(viewer_url);

        assert_eq!(keyboard.inline_keyboard.len(), 1);
        assert_eq!(keyboard.inline_keyboard[0].len(), 1);
        assert_eq!(keyboard.inline_keyboard[0][0].text, "Open html");

        Ok(())
    }
}
