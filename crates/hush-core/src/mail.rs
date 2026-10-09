use crate::error::Error;

/// The suffix used when a vault has no name of its own.
///
/// Mail is members-only and never leaves Hush: the address is a display
/// convention, not an MX record. The suffix is the vault's own name, chosen
/// when the vault is created; this default keeps older vaults (and vaults
/// created without a name) rendering a stable address.
pub const DEFAULT_VAULT_NAME: &str = "hush";

pub const MAX_HANDLE_LEN: usize = 64;

/// Validate and canonicalize a local part.
///
/// Lowercase, `[a-z0-9._-]`, 1..=64 chars, must start and end alphanumeric, no
/// `..` — the conservative email-local-part shape. A handle is a label bound to
/// a key, so two handles that differ only by case would be a real ambiguity.
pub fn normalize_handle(input: &str) -> Result<String, Error> {
    let h = input.trim().to_ascii_lowercase();
    if h.is_empty() || h.len() > MAX_HANDLE_LEN {
        return Err(Error::InvalidHandle);
    }
    let alnum = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
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

/// Validate and canonicalize a vault name used as the mail suffix.
///
/// The same shape as a handle — the suffix is the right-hand side of the same
/// display convention, so it obeys the same rules. The vault's display name is
/// this slug; there is deliberately no second, prettier form to keep in sync.
pub fn normalize_vault_name(input: &str) -> Result<String, Error> {
    normalize_handle(input)
}

/// Render the display address: the handle, at the vault's own name.
///
/// `name@vaultname` rather than `name@domain` on purpose — the suffix is
/// user-chosen vault state, not infrastructure, so it must not look like a
/// routable domain. Nothing here resolves; mail never leaves Hush.
pub fn format_address(handle: &str, vault_name: &str) -> String {
    format!("{handle}@{vault_name}")
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
    fn a_vault_name_follows_the_same_shape_as_a_handle() {
        assert_eq!(normalize_vault_name("  Quiet-Harbor ").unwrap(), "quiet-harbor");
        assert!(normalize_vault_name("has space").is_err());
    }

    #[test]
    fn an_address_is_the_handle_at_the_vault_name() {
        assert_eq!(format_address("sandy", "quiet-harbor"), "sandy@quiet-harbor");
    }
}
