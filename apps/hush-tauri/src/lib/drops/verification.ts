/**
 * Drop verification.
 *
 * The rule this file exists to enforce: an onion address is only usable when it
 * has been checked against a source we trust, and it is only ever a v3 onion.
 * A Drop with no verified address can be shown in the directory but cannot be
 * opened.
 *
 * Either scheme is accepted on an onion. SecureDrop serves plain `http` — the
 * onion service layer already authenticates the address and encrypts the link,
 * so requiring `https` would refuse every real instance.
 */

import type { Drop } from "./types";

/** The canonical, human-curated directory of SecureDrop instances. */
export const SECUREDROP_DIRECTORY_URL = "https://securedrop.org/directory/";

/** Where a verification claim came from. */
export type VerificationSource =
  | "officialWebsite"
  | "securedropDirectory"
  | "organizationSite";

/** A v3 onion host is 56 base32 characters and the `.onion` suffix. */
const V3_ONION_HOST = /^[a-z2-7]{56}\.onion$/;

/** True when `host` is a syntactically valid v3 onion host. */
export function isValidOnionHost(host: string): boolean {
  return V3_ONION_HOST.test(host.toLowerCase());
}

/**
 * The onion host of a URL, or `null` when it is not an http(s) v3 onion. Used
 * both to gate opening and to keep the raw address out of the card face.
 */
export function onionHost(url: string | null): string | null {
  if (!url) return null;
  try {
    const parsed = new URL(url);
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") return null;
    const host = parsed.hostname.toLowerCase();
    return isValidOnionHost(host) ? host : null;
  } catch {
    return null;
  }
}

/** A Drop can be opened only when verified and carrying a valid v3 onion. */
export function isOpenable(drop: Drop): boolean {
  if (drop.devOnly) return !!drop.devTarget;
  return drop.verified && onionHost(drop.onionAddress) !== null;
}

/** The short badge text for a Drop's verification state. */
export function verificationLabel(drop: Drop): string {
  if (drop.devOnly) return "Development";
  if (drop.verified && onionHost(drop.onionAddress)) return "Verified";
  return "Unverified";
}

/** The longer explanation shown under the badge. */
export function verificationDetail(drop: Drop): string {
  if (drop.devOnly) {
    return "Test entry. Points at a local mock, not a real SecureDrop.";
  }
  if (drop.verified && onionHost(drop.onionAddress)) {
    return drop.verificationSource
      ? `Address verified via ${drop.verificationSource}.`
      : "Address verified.";
  }
  return "Address not independently verified. Not openable until it is.";
}
