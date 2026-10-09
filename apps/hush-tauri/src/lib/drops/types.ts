/**
 * The Drop model.
 *
 * A Drop is an anonymous submission destination — usually a SecureDrop `.onion`
 * run by a newsroom or NGO. It is deliberately separate from the encrypted
 * outsider-mail "drop" in `hush-core`: that one is a container Hush produces,
 * this one is a third-party website Hush only opens a window onto.
 */

/** The submission protocol a Drop speaks. */
export type DropProtocol = "SecureDrop" | "GlobaLeaks";

export interface Drop {
  /** Stable id used to correlate the open window with a card. */
  id: string;
  /** The short name shown on the card. */
  name: string;
  /** One or two sentences, written by us, not scraped. */
  description: string;
  /** The organisation that runs the Drop. */
  organization: string;
  /** e.g. "News", "Human rights", "Whistleblowing". */
  category: string;
  /** e.g. "International", "United Kingdom". */
  country: string;
  protocol: DropProtocol;
  /**
   * The v3 `.onion` URL, or `null` until it has been independently verified.
   * Never invent one: an unverified address is worse than a missing one.
   */
  onionAddress: string | null;
  /** The organisation's clearnet site, for verification and reference. */
  website: string | null;
  /** Where the onion address was verified from, or `null` if not yet verified. */
  verificationSource: string | null;
  /** True only when `onionAddress` was checked against a trusted source. */
  verified: boolean;
  /**
   * A development/test entry. Never a real destination; opened against a local
   * mock and clearly labelled in the UI.
   */
  devOnly?: boolean;
  /** For `devOnly` entries: the local mock URL to open instead of an onion. */
  devTarget?: string;
}
