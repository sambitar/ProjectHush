# Threat model (v1)

Plain language. If we cannot say it here, we do not say it on the website.

## What we are protecting

- **Message and file contents** from the company, the relay host, the cloud replica host, and other users who were not invited.
- **The vault at rest** on a locked device (passphrase/PIN; auto-lock).
- **Who talks to whom**, as seen by *our relay*, so it is not a social-graph database. The relay stores blobs on random queue IDs, not “Alice’s inbox.”

## Who we treat as an attacker

- Us (operators), VPS host, BYO cloud.
- A roommate or thief who finds a locked laptop/phone.
- A random person on the internet who does not have an invite slot.
- A contact who was blocked (they should not keep delivering to a live slot).

## Who we do not claim to beat

- Someone who already has the unlocked app or the passphrase.
- Malware on the device.
- A government that seizes the device and compels keys, or watches all network traffic (ISP-level correlation of sizes and times).
- A thief of the **update signing key** (that person can ship a malicious app). Protect that key.

## Assets

- Identity keys and Signal session state
- Vault (messages, files, local nicknames)
- Recovery code
- Invite/queue tokens
- Update signing key (operator)

## Trust

- **Device (when unlocked):** full access. That is the product.
- **Relay:** ciphertext, size, time, slot ID, IP. Must not learn names or a person directory.
- **Cloud replica:** ciphertext of the vault. Unlock is the user’s passphrase.
- **Payment processor:** name/email/money. The messenger identity is not the Stripe customer ID.
- **Update server:** can offer bytes; the app installs only if they verify the baked-in public key.

## Implications already decided

- No phone/email directory, no global username search.
- Add-by QR/invite.
- Report-a-slot, not content scanning.
- Unpaid users keep the local vault.
