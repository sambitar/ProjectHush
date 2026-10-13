//! The one door to the public network.

use async_trait::async_trait;

use crate::nip22::KIND_COMMENT;
use crate::nip23::{RawEvent, KIND_LONG_FORM};

/// What to ask the relays for.
///
/// The lane builds exactly two filters — [`Filter::long_form`] and
/// [`Filter::comments`] — and neither can express `kind:1`, so a short note
/// cannot be fetched by construction.
///
/// `tags` is NIP-01's `#<name>` matching: every name present must match at least
/// one value, and several values under one name are a union. That is how a whole
/// set of threads is read in one request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filter {
    pub kinds: Vec<u64>,
    /// Tag filters, `(name, values)`. Empty for a filter that matches on kind
    /// alone.
    pub tags: Vec<(String, Vec<String>)>,
    pub limit: usize,
}

impl Filter {
    /// The article filter: long-form, newest first, capped.
    pub fn long_form(limit: usize) -> Self {
        Self {
            kinds: vec![KIND_LONG_FORM],
            tags: Vec::new(),
            limit,
        }
    }

    /// The reply filter: comments whose **root** is one of `scopes`, each an
    /// article address `30023:pubkey:d`.
    ///
    /// `A`, not `a`: NIP-22 scopes every comment in a thread to the article with
    /// the uppercase root tag, whether it answers the article or another
    /// comment, so one tag name reaches the whole thread. Matching on the
    /// lowercase parent instead would return only the top-level comments and
    /// silently drop every reply.
    ///
    /// An empty `scopes` is an empty filter, which the caller must not send: a
    /// REQ with `#A: []` matches nothing, but it is still a request to a public
    /// relay. [`crate::refresh`] skips the pass instead.
    pub fn comments(scopes: &[String], limit: usize) -> Self {
        Self {
            kinds: vec![KIND_COMMENT],
            tags: vec![("A".to_string(), scopes.to_vec())],
            limit,
        }
    }
}

/// The daemon's one door to the public network.
///
/// The transport lives behind this seam so the lane's logic — gate, dedupe,
/// cache, index — is testable without a relay, and so the Tor client can change
/// without touching the lane. A source returns raw events and nothing more; the
/// gate decides what is an article.
#[async_trait]
pub trait RelaySource: Send + Sync {
    /// Fetch events matching `filter` from the allowlisted relays.
    ///
    /// `relays` is the caller's allowlist: the source holds none of its own, so
    /// the list the user edits is the list that is read from.
    async fn fetch(&self, relays: &[String], filter: &Filter) -> Result<Vec<RawEvent>, String>;
}

/// What the relays said about one published event.
///
/// Publishing is not a broadcast that either works or does not: relays accept
/// and refuse individually, and a member needs to know which. An event one relay
/// took is published; an event no relay took is not, and the reasons are the
/// only thing that can be acted on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PublishReport {
    /// The relays that answered `OK` with `true`. A relay that already had the
    /// event is in here: NIP-23 is editable by address, so a re-export landing
    /// on a relay that holds the previous revision is the ordinary case.
    pub accepted: Vec<String>,
    /// The relays that failed, closed, or answered `OK` with `false`, each with
    /// the reason it gave.
    pub refused: Vec<String>,
}

impl PublishReport {
    /// Whether anyone took it. An event no relay accepted is not published.
    pub fn published(&self) -> bool {
        !self.accepted.is_empty()
    }

    /// One line naming what happened, for a caller that has to say something.
    pub fn summary(&self) -> String {
        if self.accepted.is_empty() {
            if self.refused.is_empty() {
                return "no relay accepted the event: no relays were given".to_string();
            }
            return format!("no relay accepted the event: {}", self.refused.join("; "));
        }
        let mut out = format!("published to {} relay(s)", self.accepted.len());
        if !self.refused.is_empty() {
            out.push_str(&format!("; refused by {}", self.refused.join("; ")));
        }
        out
    }
}

/// The write half of the one door.
///
/// Separate from [`RelaySource`] because the two are not the same capability and
/// must not be: a lane that can read the network is not a lane that can speak on
/// the member's behalf. The daemon wires them independently, and a test that
/// needs a publish without a network injects only this.
#[async_trait]
pub trait RelaySink: Send + Sync {
    /// Send one signed event to the allowlisted relays.
    ///
    /// `Err` only when nothing accepted it. A partial acceptance is `Ok`, with
    /// the refusals carried in the report: one relay being down is not a reason
    /// to tell the author their export failed when it is readable.
    async fn publish(
        &self,
        relays: &[String],
        event: &crate::event::Event,
    ) -> Result<PublishReport, String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Neither filter the lane builds can ask a relay for a short note.
    #[test]
    fn neither_filter_this_lane_builds_can_be_a_note() {
        let articles = Filter::long_form(200);
        assert_eq!(articles.kinds, vec![KIND_LONG_FORM]);
        assert!(articles.tags.is_empty());
        assert_eq!(articles.limit, 200);

        let replies = Filter::comments(&["30023:ab:one".to_string()], 100);
        assert_eq!(replies.kinds, vec![KIND_COMMENT]);
        assert_eq!(
            replies.tags,
            vec![(
                "A".to_string(),
                vec!["30023:ab:one".to_string()]
            )],
            "the root tag, so the whole thread comes back rather than only its \
             top-level comments"
        );
        assert_eq!(replies.limit, 100);
    }

    #[test]
    fn a_report_says_whether_anyone_took_the_event() {
        let nothing = PublishReport::default();
        assert!(!nothing.published(), "an empty report published nothing");
        assert_eq!(
            nothing.summary(),
            "no relay accepted the event: no relays were given"
        );

        let one = PublishReport {
            accepted: vec!["ws://a".to_string()],
            refused: Vec::new(),
        };
        assert!(one.published());
        assert_eq!(one.summary(), "published to 1 relay(s)");

        // A partial acceptance is still published, and the summary says so
        // without hiding the refusal.
        let partial = PublishReport {
            accepted: vec!["ws://a".to_string(), "ws://b".to_string()],
            refused: vec!["ws://c: blocked".to_string()],
        };
        assert!(partial.published());
        assert_eq!(
            partial.summary(),
            "published to 2 relay(s); refused by ws://c: blocked"
        );
    }
}

