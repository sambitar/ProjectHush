import { icon } from "../../lib/icons";

/**
 * The "End-to-end encrypted" mark: a lock and the words, in one place so every
 * surface that carries member-to-member content makes the same claim in the
 * same shape.
 *
 * It is a claim about the transport, not decoration, so a surface may only use
 * it when the claim is true of that surface — see `docs/PROTOCOL.md`. Chat,
 * files, mail, conclaves and the paper all ride the 1:1 Signal session; voice
 * is DTLS-SRTP between the two daemons with the SDP fingerprints authenticated
 * by that same session; a drop is sealed to the recipient's published key
 * before it is parked. In every one of those cases the relay holds ciphertext
 * it has no key for.
 *
 * Surfaces that cannot make the claim, and so must not use this: Reads,
 * Torrents and IRC talk to services outside the members-only envelope, and
 * Notes never leave the vault at all.
 */
export function e2eMark(): HTMLElement {
  const el = document.createElement("span");
  el.className = "e2e-mark";
  el.append(icon("lock"), document.createTextNode("End-to-end encrypted"));
  el.title = "The relay only ever holds ciphertext it cannot read.";
  return el;
}
