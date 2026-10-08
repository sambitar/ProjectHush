//! The notification seam.
//!
//! A drop's payload never goes through email — only a link to the pickup page
//! does. That link is the one thing the product cannot deliver over its own
//! transport, because the recipient is not a member, so it goes out through a
//! `Mailer`. Today that is a no-op or a file; a real SMTP client implements the
//! same trait later and nothing above it changes.

use std::path::PathBuf;

use crate::error::Error;

/// Deliver one plain-text notification.
///
/// Synchronous on purpose: a notification is a single small message, and the
/// daemon already runs the send off the UI thread. An async trait would need
/// `async_trait` or a boxed future for a `Box<dyn Mailer>`, which is a lot of
/// machinery for one method.
///
/// Note what is *not* a parameter: the PIN. A notification can only carry the
/// link and an expiry, because that is all it is given. The PIN is shared out of
/// band by the sender, and the type makes leaking it into an email impossible
/// rather than merely discouraged.
pub trait Mailer: Send + Sync {
    fn send(&self, to: &str, subject: &str, body: &str) -> Result<(), Error>;
}

/// Discards everything. The default, so a vault with no mail configured still
/// sends drops — the sender copies the link and passes it on by hand.
pub struct NullMailer;

impl Mailer for NullMailer {
    fn send(&self, _to: &str, _subject: &str, _body: &str) -> Result<(), Error> {
        Ok(())
    }
}

/// Writes each notification to a file in a directory.
///
/// For local dogfood and tests: it makes the notification observable without an
/// SMTP server, which is exactly what is needed to check that the link and the
/// expiry are right and that no PIN is present.
pub struct FileMailer {
    dir: PathBuf,
}

impl FileMailer {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }
}

impl Mailer for FileMailer {
    fn send(&self, to: &str, subject: &str, body: &str) -> Result<(), Error> {
        std::fs::create_dir_all(&self.dir)?;
        let name = format!(
            "{}-{}.txt",
            now_secs(),
            hex::encode(crate::crypto::random_bytes::<4>())
        );
        std::fs::write(
            self.dir.join(name),
            format!("To: {to}\nSubject: {subject}\n\n{body}\n"),
        )?;
        Ok(())
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// "in 7 days", "in 3 hours", "in 20 minutes", or "soon" once it has passed.
///
/// Rounded up, not down: a seven-day link that has just been created must read
/// "in 7 days". Truncating would say "in 6 days" a second after it was sent,
/// which makes the sender think the link is shorter-lived than it is.
///
/// Relative rather than an absolute date: the recipient needs to know how long
/// they have, not a timestamp they must convert, and this needs no calendar
/// dependency.
fn expiry_phrase(expires_unix: u64, now: u64) -> String {
    let secs = expires_unix.saturating_sub(now);
    if secs == 0 {
        return "soon".to_string();
    }
    if secs >= 86_400 {
        let days = secs.div_ceil(86_400);
        return format!("in {days} day{}", if days == 1 { "" } else { "s" });
    }
    if secs >= 3_600 {
        let hours = secs.div_ceil(3_600);
        return format!("in {hours} hour{}", if hours == 1 { "" } else { "s" });
    }
    let minutes = (secs / 60).max(1);
    format!("in {minutes} minute{}", if minutes == 1 { "" } else { "s" })
}

/// The notification body for a PIN drop: a link, an expiry, and a warning that a
/// PIN is needed.
///
/// Deliberately plain text with no HTML, so nothing in it can be rendered as a
/// link that is not the one link, and no tracking pixel has anywhere to hide.
pub fn notification_body(link: &str, expires_unix: u64) -> String {
    notification(
        link,
        expires_unix,
        "You will need the delivery PIN they gave you separately. \
         It is not in this email, and it cannot be recovered from this link.",
    )
}

/// The notification body for a drop sealed to a public key.
///
/// There is no PIN to look for here: the recipient opens it with their private
/// key. Telling them to find a PIN would send them looking for a secret that does
/// not exist, so only the instruction differs; the shape (link and expiry) is the
/// same, and the key still never appears in the message.
pub fn notification_body_for_key(link: &str, expires_unix: u64) -> String {
    notification(
        link,
        expires_unix,
        "Open it with your private key. No PIN is needed, and this email cannot \
         decrypt it on its own.",
    )
}

fn notification(link: &str, expires_unix: u64, instruction: &str) -> String {
    format!(
        "Someone sent you an encrypted message.\n\n\
         Open it here:\n\
         {link}\n\n\
         {instruction}\n\n\
         The link expires {}.\n",
        expiry_phrase(expires_unix, now_secs())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_notification_carries_the_link_and_says_a_pin_is_needed() {
        let body = notification_body("https://x.example/open/abc", now_secs() + 7 * 86_400);
        assert!(body.contains("https://x.example/open/abc"));
        assert!(
            body.to_lowercase().contains("delivery pin"),
            "the recipient must be told a PIN is required"
        );
        assert!(body.contains("in 7 days"));
    }

    #[test]
    fn the_key_notification_does_not_send_the_recipient_looking_for_a_pin() {
        let body = notification_body_for_key("https://x.example/open/abc", now_secs() + 7 * 86_400);
        assert!(body.contains("https://x.example/open/abc"));
        assert!(body.contains("in 7 days"));
        assert!(body.to_lowercase().contains("private key"));
        assert!(
            !body.to_lowercase().contains("delivery pin"),
            "a key drop has no PIN, so the notification must not mention one: {body}"
        );
    }

    #[test]
    fn the_expiry_phrase_degrades_gracefully() {
        let now = 1_000_000u64;
        assert_eq!(expiry_phrase(now + 2 * 86_400, now), "in 2 days");
        assert_eq!(expiry_phrase(now + 86_400, now), "in 1 day");
        assert_eq!(expiry_phrase(now + 7_200, now), "in 2 hours");
        assert_eq!(expiry_phrase(now + 300, now), "in 5 minutes");
        assert_eq!(expiry_phrase(now, now), "soon");
        // An already-expired link must not underflow into a huge number.
        assert_eq!(expiry_phrase(now - 5_000, now), "soon");
        // A seven-day link is "in 7 days" the instant it is created, not 6: this
        // is the rounding the sender actually sees.
        assert_eq!(expiry_phrase(now + 7 * 86_400, now), "in 7 days");
        assert_eq!(expiry_phrase(now + 7 * 86_400 - 1, now), "in 7 days");
        // Rounding up must not turn a past-the-hour remainder into an extra hour
        // for something that has only seconds left.
        assert_eq!(expiry_phrase(now + 30, now), "in 1 minute");
    }

    #[test]
    fn the_file_mailer_writes_one_message_and_the_null_mailer_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        FileMailer::new(dir.path())
            .send("bob@gmail.com", "subject", "body")
            .unwrap();
        let written: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(written.len(), 1);

        let contents = std::fs::read_to_string(written[0].as_ref().unwrap().path()).unwrap();
        assert!(contents.contains("To: bob@gmail.com"));
        assert!(contents.contains("body"));

        assert!(NullMailer.send("bob@gmail.com", "s", "b").is_ok());
    }

    /// The property the email design rests on: a notification cannot contain a
    /// PIN, because it is never handed one.
    #[test]
    fn a_notification_cannot_leak_a_pin() {
        let pin = "4821";
        let body = notification_body("https://x.example/open/abc", now_secs() + 60);
        assert!(!body.contains(pin), "the PIN value must not appear");
    }
}
