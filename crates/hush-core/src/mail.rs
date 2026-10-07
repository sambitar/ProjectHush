use crate::error::Error;

/// Fixed domain suffix for internal member mail.
///
/// Members-only mail never leaves Hush: this is a display convention, not an MX
/// record. It is a constant rather than per-vault config so two members on the
/// same relay always render the same address, and the UI reads it from
/// `/api/status` instead of hardcoding a second copy.
pub const MAIL_DOMAIN: &str = "myserver.com";

pub const MAX_HANDLE_LEN: usize = 64;

/// Validate and canonicalize a local part.
///
/// Lowercase, `[a-z0-9._-]`, 1..=64 chars, must start and end alphanumeric, no
/// `..` — the conservative email-local-part shape. A handle is a label bound to
/// a key, so two handles that differ only by case would be a real ambiguity.
pub fn normalize_handle(input: &str) -> Result<String, Error> {
    let h = input.trim().to_ascii_lowercase();
    let alnum = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    if h.is_empty() || h.len() > MAX_HANDLE_LEN {
        return Err(Error::InvalidHandle);
    }
    let allowed = h
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'));
    if !allowed
        || !h.chars().next().is_some_and(alnum)
        || !h.chars().last().is_some_and(alnum)
        || h.contains("..")
    {
        return Err(Error::InvalidHandle);
    }
    Ok(h)
}

pub fn format_address(handle: &str) -> String {
    format!("{handle}@{MAIL_DOMAIN}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_are_lowercased_and_trimmed() {
        assert_eq!(normalize_handle("  Sandy ").unwrap(), "sandy");
    }

    #[test]
    fn handles_reject_the_shapes_that_would_be_ambiguous() {
        for bad in [
            "", "a b", "sandy@x", "..", ".sandy", "sandy.", "-s", "s-", "s/a", "s'andy",
        ] {
            assert!(normalize_handle(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn a_valid_handle_formats_with_the_domain() {
        assert_eq!(format_address("sandy"), "sandy@myserver.com");
    }
}
