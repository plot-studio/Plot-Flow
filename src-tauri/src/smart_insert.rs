//! Smart insert: paste dictation only where the user is typing.
//!
//! Before every paste (live-insert phrases and the end-of-recording paste)
//! the keyboard focus is probed ([`crate::focus`]). If the cursor is in a
//! text field — or we cannot tell — the text is pasted as before. If it is
//! clearly not (desktop, a list, a web page body, a minimized app, our own
//! window), the text is *held*: kept in memory, put on the clipboard and the
//! user is told to press Ctrl+V. Text held during the current recording is
//! pasted first, together with the next phrase, as soon as the user is back
//! in a text field, so nothing is lost while they move around.
//!
//! Held text from an earlier recording is not pasted again automatically —
//! the user was already told to press Ctrl+V and may have done so; it stays
//! on the clipboard and in history.

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use log::{error, info, warn};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::focus;
use crate::managers::live_insert::joiner;
use crate::settings::{get_settings, PasteMethod};

/// Held text older than this is dropped (history keeps it anyway).
pub(crate) const PENDING_TTL: Duration = Duration::from_secs(30 * 60);

/// Result of [`deliver`].
#[derive(Debug)]
pub enum Delivery {
    /// Pasted at the cursor (together with any held text of this recording).
    Pasted,
    /// Not pasted: kept in memory and on the clipboard.
    Held,
    /// Smart insert is off and the paste failed (old behaviour).
    Failed(String),
}

/// Payload of the `pending-text` event: how many characters are held.
/// `0` means the held text was pasted or dropped.
#[derive(Clone, Serialize)]
pub struct PendingTextEvent {
    pub chars: usize,
}

/// Text that could not be pasted yet.
#[derive(Debug)]
pub(crate) struct PendingBuffer {
    text: String,
    last_piece: Option<String>,
    updated: Option<Instant>,
    /// Held during the current recording: flushed with the next paste.
    fresh: bool,
}

impl PendingBuffer {
    pub(crate) const fn new() -> Self {
        Self {
            text: String::new(),
            last_piece: None,
            updated: None,
            fresh: false,
        }
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn chars(&self) -> usize {
        self.text.chars().count()
    }

    pub(crate) fn clear(&mut self) {
        self.text.clear();
        self.last_piece = None;
        self.updated = None;
        self.fresh = false;
    }

    /// Drop text that has waited longer than [`PENDING_TTL`].
    pub(crate) fn expire(&mut self, now: Instant) -> bool {
        match self.updated {
            Some(at) if now.saturating_duration_since(at) >= PENDING_TTL => {
                self.clear();
                true
            }
            _ => false,
        }
    }

    /// A new recording starts: earlier held text is no longer auto-pasted.
    pub(crate) fn begin_session(&mut self) {
        self.fresh = false;
    }

    /// Keep `piece` (raw phrase text, no leading joiner).
    pub(crate) fn hold(&mut self, piece: &str, now: Instant) {
        if !self.fresh {
            // Text from an earlier recording was already announced; start over
            // so Ctrl+V does not repeat it.
            self.clear();
            self.fresh = true;
        }
        self.text
            .push_str(joiner(self.last_piece.as_deref(), piece));
        self.text.push_str(piece);
        self.last_piece = Some(piece.to_string());
        self.updated = Some(now);
    }

    /// What to paste for `piece`: held text of this recording first, then the
    /// piece; otherwise the piece with the caller's usual `lead` joiner.
    pub(crate) fn paste_payload(&self, piece: &str, lead: &str) -> String {
        if self.fresh && !self.text.is_empty() {
            format!(
                "{}{}{}",
                self.text,
                joiner(self.last_piece.as_deref(), piece),
                piece
            )
        } else {
            format!("{lead}{piece}")
        }
    }

    /// Text was held during this recording and not pasted since.
    pub(crate) fn notice_due(&self) -> bool {
        self.fresh && !self.text.is_empty()
    }
}

static PENDING: Mutex<PendingBuffer> = Mutex::new(PendingBuffer::new());

fn pending() -> MutexGuard<'static, PendingBuffer> {
    PENDING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Call when a recording starts delivering text.
pub fn begin_session() {
    pending().begin_session();
}

/// Paste `text` at the cursor if the cursor is in a text field, else hold it.
///
/// `lead` is what the caller would put before `text` when pasting it right
/// after its previous piece (live insert's joiner; `""` for a whole
/// dictation). Must be called where `clipboard::paste` may run (the main
/// thread).
pub fn deliver(app: &AppHandle, text: &str, lead: &str) -> Delivery {
    let settings = get_settings(app);
    if !settings.smart_insert || settings.paste_method == PasteMethod::None {
        return match crate::clipboard::paste(format!("{lead}{text}"), app.clone()) {
            Ok(()) => Delivery::Pasted,
            Err(e) => Delivery::Failed(e),
        };
    }

    let now = Instant::now();
    let target = focus::focused_text_target();
    if !target.accepts_paste() {
        return hold(app, text, now, "cursor is not in a text field");
    }

    let (payload, had_pending) = {
        let mut buffer = pending();
        buffer.expire(now);
        (buffer.paste_payload(text, lead), !buffer.text().is_empty())
    };
    match crate::clipboard::paste(payload, app.clone()) {
        Ok(()) => {
            if had_pending {
                pending().clear();
                emit_pending(app, 0);
            }
            Delivery::Pasted
        }
        Err(e) => {
            error!("Smart insert: paste failed: {e}");
            hold(app, text, now, "paste failed")
        }
    }
}

fn hold(app: &AppHandle, text: &str, now: Instant, why: &str) -> Delivery {
    let (full, chars) = {
        let mut buffer = pending();
        buffer.expire(now);
        buffer.hold(text, now);
        (buffer.text().to_string(), buffer.chars())
    };
    info!("Smart insert: holding {chars} chars ({why}); they are on the clipboard");
    if let Err(e) = crate::clipboard::copy_text_to_clipboard(app, &full) {
        warn!("Smart insert: cannot copy held text to the clipboard: {e}");
    }
    emit_pending(app, chars);
    Delivery::Held
}

fn emit_pending(app: &AppHandle, chars: usize) {
    let _ = app.emit("pending-text", PendingTextEvent { chars });
}

/// End of a recording: if text was held during it, leave the overlay up with
/// "Text saved — press Ctrl+V"; otherwise hide the overlay as usual.
pub fn finish_overlay(app: &AppHandle) {
    let due = pending().notice_due();
    if due {
        crate::overlay::show_saved_notice(app);
    } else {
        crate::utils::hide_recording_overlay(app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_buffer_pastes_with_callers_joiner() {
        let buffer = PendingBuffer::new();
        assert_eq!(buffer.paste_payload("и дальше", " "), " и дальше");
        assert!(!buffer.notice_due());
    }

    #[test]
    fn held_phrases_are_joined() {
        let now = Instant::now();
        let mut buffer = PendingBuffer::new();
        buffer.hold("Сделай сайт", now);
        buffer.hold("Потом посмотрим", now);
        buffer.hold("и обсудим", now);
        assert_eq!(buffer.text(), "Сделай сайт. Потом посмотрим и обсудим");
        assert!(buffer.notice_due());
    }

    #[test]
    fn fresh_pending_is_flushed_before_the_new_phrase() {
        let now = Instant::now();
        let mut buffer = PendingBuffer::new();
        buffer.hold("Первая фраза", now);
        assert_eq!(
            buffer.paste_payload("Вторая фраза", " "),
            "Первая фраза. Вторая фраза"
        );
        assert_eq!(buffer.paste_payload("и ещё", " "), "Первая фраза и ещё");
    }

    #[test]
    fn earlier_recording_is_not_flushed_again() {
        let now = Instant::now();
        let mut buffer = PendingBuffer::new();
        buffer.hold("Старый текст", now);
        buffer.begin_session();
        assert!(!buffer.notice_due());
        assert_eq!(buffer.paste_payload("Новый", ""), "Новый");
        // Holding in the new recording replaces the announced old text.
        buffer.hold("Новый", now);
        assert_eq!(buffer.text(), "Новый");
    }

    #[test]
    fn pending_expires_after_ttl() {
        let start = Instant::now();
        let mut buffer = PendingBuffer::new();
        buffer.hold("Текст", start);
        assert!(!buffer.expire(start + Duration::from_secs(60)));
        assert_eq!(buffer.text(), "Текст");
        assert!(buffer.expire(start + PENDING_TTL));
        assert_eq!(buffer.text(), "");
        assert!(!buffer.notice_due());
    }

    #[test]
    fn clear_resets_everything() {
        let now = Instant::now();
        let mut buffer = PendingBuffer::new();
        buffer.hold("Текст", now);
        buffer.clear();
        assert_eq!(buffer.chars(), 0);
        assert_eq!(buffer.paste_payload("Новый", ""), "Новый");
    }
}
