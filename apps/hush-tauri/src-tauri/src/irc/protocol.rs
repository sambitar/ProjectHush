//! Raw IRC line parsing.
//!
//! One function, [`ParsedLine::parse`], turns a wire line into tags, an
//! optional prefix, the command, and its parameters. Everything the client
//! does downstream (dispatch, NAMES, numerics) reads this struct rather than
//! re-splitting strings.

/// A parsed IRC protocol line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedLine {
    /// IRCv3 message tags (`@key=value;...`), unescaped.
    pub tags: Vec<(String, String)>,
    /// The prefix (`nick!user@host` or a server name), without the leading `:`.
    pub prefix: Option<String>,
    /// The command, uppercased (`PRIVMSG`, `353`, ...).
    pub command: String,
    /// The parameters, with the trailing parameter's leading `:` removed.
    pub params: Vec<String>,
}

impl ParsedLine {
    pub fn parse(line: &str) -> Self {
        let line = line.trim_end_matches(['\r', '\n']);
        let mut rest = line;

        let mut tags = Vec::new();
        if let Some(after) = rest.strip_prefix('@') {
            let (tag_part, remainder) = split_first(after);
            tags = parse_tags(tag_part);
            rest = remainder;
        }

        let mut prefix = None;
        if let Some(after) = rest.strip_prefix(':') {
            let (prefix_part, remainder) = split_first(after);
            prefix = Some(prefix_part.to_string());
            rest = remainder;
        }

        let (command, remainder) = split_first(rest);
        let mut params = Vec::new();
        let mut r = remainder;
        while !r.is_empty() {
            if let Some(trailing) = r.strip_prefix(':') {
                params.push(trailing.to_string());
                break;
            }
            let (token, next) = split_first(r);
            params.push(token.to_string());
            r = next;
        }

        ParsedLine {
            tags,
            prefix,
            command: command.to_ascii_uppercase(),
            params,
        }
    }

    /// The nickname in the prefix, if the prefix is a user (`nick!user@host`).
    pub fn nick(&self) -> Option<&str> {
        let prefix = self.prefix.as_deref()?;
        let nick = prefix.split(['!', '@']).next().unwrap_or(prefix);
        (!nick.is_empty()).then_some(nick)
    }

    /// The first parameter (the message target for `PRIVMSG`, the channel for
    /// `JOIN`, ...).
    pub fn param(&self, index: usize) -> Option<&str> {
        self.params.get(index).map(String::as_str)
    }

    /// The last parameter, which is the trailing one for most commands.
    pub fn last(&self) -> Option<&str> {
        self.params.last().map(String::as_str)
    }

    /// The value of one message tag, if present. Part of the parser's surface;
    /// the client does not request message-tags yet, hence the allow.
    #[allow(dead_code)]
    pub fn tag(&self, key: &str) -> Option<&str> {
        self.tags
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// Split off the first whitespace-delimited token. Returns `(token, rest)`
/// where `rest` has leading spaces trimmed (IRC uses single spaces, but a
/// stray one from a hand-typed `/raw` should not become an empty parameter).
fn split_first(s: &str) -> (&str, &str) {
    match s.find(' ') {
        Some(i) => (&s[..i], s[i + 1..].trim_start_matches(' ')),
        None => (s, ""),
    }
}

/// Parse an IRCv3 tag section (`key=value;key2`), unescaping values.
fn parse_tags(raw: &str) -> Vec<(String, String)> {
    raw.split(';')
        .filter(|s| !s.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) => (k.to_string(), unescape_tag(v)),
            None => (pair.to_string(), String::new()),
        })
        .collect()
}

/// IRCv3 tag escaping: `\:` -> `;`, `\s` -> space, `\\` -> `\`, `\r`, `\n`.
fn unescape_tag(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some(':') => out.push(';'),
            Some('s') => out.push(' '),
            Some('\\') => out.push('\\'),
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// The CTCP command and argument when `text` is a single CTCP message
/// (`\x01ACTION waves\x01`). Returns `None` for ordinary text.
pub fn parse_ctcp(text: &str) -> Option<(&str, &str)> {
    let inner = text.strip_prefix('\u{1}')?.strip_suffix('\u{1}')?;
    let (cmd, arg) = match inner.find(' ') {
        Some(i) => (&inner[..i], inner[i + 1..].trim_start_matches(' ')),
        None => (inner, ""),
    };
    Some((cmd, arg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_privmsg_with_colons_in_the_message() {
        let line = ":alice!u@h PRIVMSG #room :hello: world, meet bob";
        let p = ParsedLine::parse(line);
        assert_eq!(p.command, "PRIVMSG");
        assert_eq!(p.nick(), Some("alice"));
        assert_eq!(p.param(0), Some("#room"));
        assert_eq!(p.last(), Some("hello: world, meet bob"));
    }

    #[test]
    fn parses_a_line_with_tags_and_prefix() {
        let line = "@time=2020-01-01T00:00:00Z;account=bob :bob!b@h PRIVMSG #c :hi";
        let p = ParsedLine::parse(line);
        assert_eq!(p.tag("account"), Some("bob"));
        assert_eq!(p.tag("time"), Some("2020-01-01T00:00:00Z"));
        assert_eq!(p.command, "PRIVMSG");
        assert_eq!(p.nick(), Some("bob"));
    }

    #[test]
    fn unescapes_tag_values() {
        let p = ParsedLine::parse("@k=a\\sb\\:c\\\\d :s NOTICE me :x");
        assert_eq!(p.tag("k"), Some("a b;c\\d"));
    }

    #[test]
    fn parses_numerics_with_a_trailing_param() {
        let p = ParsedLine::parse(":irc.libera.chat 353 me = #room :@alice +bob carol");
        assert_eq!(p.command, "353");
        assert_eq!(p.param(0), Some("me"));
        assert_eq!(p.param(1), Some("="));
        assert_eq!(p.param(2), Some("#room"));
        assert_eq!(p.last(), Some("@alice +bob carol"));
    }

    #[test]
    fn parses_ping_with_no_prefix() {
        let p = ParsedLine::parse("PING :irc.libera.chat");
        assert_eq!(p.command, "PING");
        assert_eq!(p.prefix, None);
        assert_eq!(p.last(), Some("irc.libera.chat"));
    }

    #[test]
    fn parses_join_part_quit_nick_topic_mode() {
        assert_eq!(ParsedLine::parse(":a!u@h JOIN :#room").param(0), Some("#room"));
        assert_eq!(
            ParsedLine::parse(":a!u@h PART #room :bye").last(),
            Some("bye")
        );
        assert_eq!(ParsedLine::parse(":a!u@h QUIT :gone").last(), Some("gone"));
        assert_eq!(ParsedLine::parse(":a!u@h NICK :bob").last(), Some("bob"));
        assert_eq!(
            ParsedLine::parse(":a!u@h TOPIC #room :new topic").last(),
            Some("new topic")
        );
        let mode = ParsedLine::parse(":a!u@h MODE #room +o bob");
        assert_eq!(mode.param(0), Some("#room"));
        assert_eq!(mode.param(1), Some("+o"));
        assert_eq!(mode.param(2), Some("bob"));
    }

    #[test]
    fn parses_nick_in_use_numeric() {
        let p = ParsedLine::parse(":irc.libera.chat 433 * alice :Nickname is already in use.");
        assert_eq!(p.command, "433");
        assert_eq!(p.param(1), Some("alice"));
    }

    #[test]
    fn detects_ctcp_action() {
        let (cmd, arg) = parse_ctcp("\u{1}ACTION waves\u{1}").unwrap();
        assert_eq!(cmd, "ACTION");
        assert_eq!(arg, "waves");
        assert!(parse_ctcp("just text").is_none());
    }

    #[test]
    fn prefix_of_a_server_is_not_a_nick() {
        // A server prefix has no `!`, so the whole thing is returned; callers
        // that care about users check for `!`. `nick()` still yields a value,
        // which is why command handling checks the command, not just the nick.
        let p = ParsedLine::parse(":irc.libera.chat 001 me :Welcome");
        assert_eq!(p.nick(), Some("irc.libera.chat"));
        assert_eq!(p.command, "001");
    }
}
