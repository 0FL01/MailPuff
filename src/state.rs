use std::{
    collections::{HashMap, HashSet},
    sync::RwLock,
};

use thiserror::Error;
use uuid::Uuid;

use crate::{mail_source::MessageRef, telegram::TelegramMessageRef};

#[derive(Debug, Default)]
pub struct RuntimeState {
    inner: RwLock<RuntimeStateInner>,
}

impl RuntimeState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mark_processed(&self, mail_ref: MessageRef) -> Result<bool, RuntimeStateError> {
        Ok(self
            .inner
            .write()
            .map_err(|_| RuntimeStateError::LockPoisoned)?
            .processed_messages
            .insert(mail_ref))
    }

    pub fn is_processed(&self, mail_ref: &MessageRef) -> Result<bool, RuntimeStateError> {
        Ok(self
            .inner
            .read()
            .map_err(|_| RuntimeStateError::LockPoisoned)?
            .processed_messages
            .contains(mail_ref))
    }

    pub fn track_message(
        &self,
        mail_ref: MessageRef,
        page_id: Uuid,
        telegram_ref: TelegramMessageRef,
    ) -> Result<(), RuntimeStateError> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| RuntimeStateError::LockPoisoned)?;

        if let Some(existing) = inner.tracked_messages.remove(&mail_ref) {
            inner.page_to_message.remove(&existing.page_id);
        }
        if let Some(existing_ref) = inner.page_to_message.remove(&page_id) {
            inner.tracked_messages.remove(&existing_ref);
        }

        inner.page_to_message.insert(page_id, mail_ref.clone());
        inner.tracked_messages.insert(
            mail_ref.clone(),
            TrackedMessage {
                mail_ref,
                page_id,
                telegram_ref,
            },
        );

        Ok(())
    }

    pub fn get_tracked(
        &self,
        mail_ref: &MessageRef,
    ) -> Result<Option<TrackedMessage>, RuntimeStateError> {
        Ok(self
            .inner
            .read()
            .map_err(|_| RuntimeStateError::LockPoisoned)?
            .tracked_messages
            .get(mail_ref)
            .cloned())
    }

    pub fn get_tracked_by_page(
        &self,
        page_id: Uuid,
    ) -> Result<Option<TrackedMessage>, RuntimeStateError> {
        let inner = self
            .inner
            .read()
            .map_err(|_| RuntimeStateError::LockPoisoned)?;
        let Some(mail_ref) = inner.page_to_message.get(&page_id) else {
            return Ok(None);
        };

        Ok(inner.tracked_messages.get(mail_ref).cloned())
    }

    pub fn tracked_messages(&self) -> Result<Vec<TrackedMessage>, RuntimeStateError> {
        Ok(self
            .inner
            .read()
            .map_err(|_| RuntimeStateError::LockPoisoned)?
            .tracked_messages
            .values()
            .cloned()
            .collect())
    }

    pub fn remove_tracked(
        &self,
        mail_ref: &MessageRef,
    ) -> Result<Option<TrackedMessage>, RuntimeStateError> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| RuntimeStateError::LockPoisoned)?;
        let removed = inner.tracked_messages.remove(mail_ref);

        if let Some(tracked) = &removed {
            inner.page_to_message.remove(&tracked.page_id);
        }

        Ok(removed)
    }

    pub fn remove_tracked_by_page(
        &self,
        page_id: Uuid,
    ) -> Result<Option<TrackedMessage>, RuntimeStateError> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| RuntimeStateError::LockPoisoned)?;
        let Some(mail_ref) = inner.page_to_message.remove(&page_id) else {
            return Ok(None);
        };

        Ok(inner.tracked_messages.remove(&mail_ref))
    }

    pub fn processed_len(&self) -> Result<usize, RuntimeStateError> {
        Ok(self
            .inner
            .read()
            .map_err(|_| RuntimeStateError::LockPoisoned)?
            .processed_messages
            .len())
    }

    pub fn tracked_len(&self) -> Result<usize, RuntimeStateError> {
        Ok(self
            .inner
            .read()
            .map_err(|_| RuntimeStateError::LockPoisoned)?
            .tracked_messages
            .len())
    }
}

#[derive(Debug, Default)]
struct RuntimeStateInner {
    processed_messages: HashSet<MessageRef>,
    tracked_messages: HashMap<MessageRef, TrackedMessage>,
    page_to_message: HashMap<Uuid, MessageRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedMessage {
    pub mail_ref: MessageRef,
    pub page_id: Uuid,
    pub telegram_ref: TelegramMessageRef,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RuntimeStateError {
    #[error("runtime state lock poisoned")]
    LockPoisoned,
}

#[cfg(test)]
mod tests {
    use crate::mail_source::MailSourceKind;

    use super::*;

    fn mail_ref(stable_id: &str) -> MessageRef {
        MessageRef::new(
            MailSourceKind::Imap,
            "imap.example.com",
            Some("INBOX".to_owned()),
            stable_id,
        )
    }

    #[test]
    fn tracks_and_removes_message_by_ref_or_page() -> Result<(), RuntimeStateError> {
        let state = RuntimeState::new();
        let first_ref = mail_ref("42");
        let second_ref = mail_ref("43");
        let first_page = Uuid::new_v4();
        let second_page = Uuid::new_v4();
        let telegram_ref = TelegramMessageRef::new(100, 200);

        state.track_message(first_ref.clone(), first_page, telegram_ref)?;
        state.track_message(second_ref.clone(), second_page, telegram_ref)?;

        assert_eq!(state.tracked_len()?, 2);
        assert_eq!(
            state.get_tracked(&first_ref)?.expect("tracked").page_id,
            first_page
        );
        assert_eq!(
            state
                .get_tracked_by_page(second_page)?
                .expect("tracked")
                .mail_ref,
            second_ref
        );

        assert!(state.remove_tracked(&first_ref)?.is_some());
        assert!(state.get_tracked_by_page(first_page)?.is_none());
        assert!(state.remove_tracked_by_page(second_page)?.is_some());
        assert_eq!(state.tracked_len()?, 0);

        Ok(())
    }

    #[test]
    fn processed_set_is_independent_from_tracking() -> Result<(), RuntimeStateError> {
        let state = RuntimeState::new();
        let mail_ref = mail_ref("42");
        let page_id = Uuid::new_v4();

        assert!(state.mark_processed(mail_ref.clone())?);
        assert!(!state.mark_processed(mail_ref.clone())?);
        state.track_message(mail_ref.clone(), page_id, TelegramMessageRef::new(100, 200))?;
        assert!(state.remove_tracked_by_page(page_id)?.is_some());

        assert!(state.is_processed(&mail_ref)?);
        assert_eq!(state.processed_len()?, 1);
        assert_eq!(state.tracked_len()?, 0);

        Ok(())
    }
}
