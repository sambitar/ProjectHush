//! NAMES entries and their ordering.
//!
//! A channel's nick list is ordered by channel privilege, exactly as mIRC
//! shows it: owners/ops first, then halfops, then voiced, then everyone else,
//! alphabetical within each group. Multi-prefix (`@+`) is preserved.

use serde::Serialize;

/// One entry in a channel's nick list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NickEntry {
    /// The nickname, without prefixes.
    pub nick: String,
    /// The channel-status prefixes, e.g. `"@+"`. Empty for a regular user.
    pub prefixes: String,
}

/// Prefixes in descending privilege order, with their rank.
const PREFIXES: [(char, i32); 5] = [('~', 5), ('&', 4), ('@', 3), ('%', 2), ('+', 1)];

fn is_prefix(c: char) -> bool {
    PREFIXES.iter().any(|(p, _)| *p == c)
}

/// Split a raw `NAMES` token (`@alice`, `+bob`, `carol`) into prefixes + nick.
pub fn parse_entry(raw: &str) -> NickEntry {
    let prefixes: String = raw.chars().take_while(|c| is_prefix(*c)).collect();
    let nick = raw[prefixes.len()..].to_string();
    NickEntry { nick, prefixes }
}

/// The privilege rank of a prefix set (higher is more privileged).
pub fn rank(prefixes: &str) -> i32 {
    prefixes
        .chars()
        .filter_map(|c| PREFIXES.iter().find(|(p, _)| *p == c).map(|(_, r)| *r))
        .max()
        .unwrap_or(0)
}

/// Whether this prefix set includes channel-operator status. Exposed for the
/// tests and for callers that gate operator actions on it.
#[allow(dead_code)]
pub fn has_op(prefixes: &str) -> bool {
    rank(prefixes) >= 3
}

/// Sort in place: rank descending, then case-insensitive name, then exact name
/// so the order is total and stable across re-sorts.
pub fn sort(entries: &mut [NickEntry]) {
    entries.sort_by(|a, b| {
        rank(&b.prefixes)
            .cmp(&rank(&a.prefixes))
            .then_with(|| {
                a.nick
                    .to_lowercase()
                    .cmp(&b.nick.to_lowercase())
            })
            .then_with(|| a.nick.cmp(&b.nick))
    });
}

/// Parse a full `NAMES` reply body (space-separated tokens) and sort it.
#[allow(dead_code)]
pub fn parse_names(body: &str) -> Vec<NickEntry> {
    let mut entries: Vec<NickEntry> = body
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .map(parse_entry)
        .collect();
    sort(&mut entries);
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_prefixed_entry() {
        let e = parse_entry("@alice");
        assert_eq!(e.nick, "alice");
        assert_eq!(e.prefixes, "@");
    }

    #[test]
    fn parses_a_multi_prefix_entry() {
        let e = parse_entry("@+alice");
        assert_eq!(e.nick, "alice");
        assert_eq!(e.prefixes, "@+");
        assert_eq!(rank(&e.prefixes), 3);
    }

    #[test]
    fn parses_a_plain_entry() {
        let e = parse_entry("carol");
        assert_eq!(e.nick, "carol");
        assert!(e.prefixes.is_empty());
        assert_eq!(rank(&e.prefixes), 0);
    }

    #[test]
    fn orders_by_prefix_then_name() {
        let v = parse_names("carol @alice +bob %dave @Zed erin");
        let order: Vec<_> = v.iter().map(|e| e.nick.as_str()).collect();
        // @ ops first (alphabetical: alice, Zed), then %dave, then +bob, then
        // regular (carol, erin).
        assert_eq!(order, ["alice", "Zed", "dave", "bob", "carol", "erin"]);
    }

    #[test]
    fn is_case_insensitive_within_a_group() {
        let v = parse_names("beta Alpha gamma");
        let order: Vec<_> = v.iter().map(|e| e.nick.as_str()).collect();
        assert_eq!(order, ["Alpha", "beta", "gamma"]);
    }

    #[test]
    fn owner_outranks_op() {
        assert!(rank("~") > rank("@"));
        assert!(rank("@") > rank("+"));
        assert!(has_op("@"));
        assert!(has_op("~"));
        assert!(!has_op("+"));
        assert!(!has_op(""));
    }

    #[test]
    fn sort_is_total_and_stable() {
        let v = parse_names("@b @a @a");
        let order: Vec<_> = v.iter().map(|e| e.nick.as_str()).collect();
        assert_eq!(order, ["a", "a", "b"]);
    }
}
