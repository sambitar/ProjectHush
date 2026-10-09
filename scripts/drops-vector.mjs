#!/usr/bin/env node
// Cross-language wire-compat vectors for hush-drop-v2 sealed boxes.
//
//   node scripts/drops-vector.mjs seal  <fixture-prefix>
//       Generates a fresh drop keypair and a sealed container with the same
//       noble stack the drop page uses, writing <prefix>.hshd plus .sk/.pk hex
//       sidecars. The Rust test `opens_a_node_sealed_container` opens the
//       result; regenerate the fixtures whenever the page's crypto deps move.
//
//   node scripts/drops-vector.mjs open  <container-file> <sk-hex> <pk-hex>
//       Opens a Rust-sealed container with the noble stack and prints the
//       subject — the other direction of the same guarantee. Run once by hand
//       after the relay drop page lands and record the output.
//
// Layout reference: crates/hush-core/src/drop.rs — HSHD magic, version 2, mode
// 2, ephemeral pk (32), nonce (24), then XChaCha20-Poly1305 ciphertext with the
// full header as AAD. Key = HKDF-SHA256(ikm = ECDH, salt = ephemeral pk,
// info = "hush-drop-v2" || recipient pk).

import { createRequire } from "node:module";
import { writeFileSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const require = createRequire(
  new URL("../crates/hush-relay/package.json", import.meta.url),
);
// In @noble/curves 1.x the X25519 curve is exported from the ed25519 module.
const { x25519 } = require("@noble/curves/ed25519");
const { xchacha20poly1305 } = require("@noble/ciphers/chacha.js");
const { hkdf } = require("@noble/hashes/hkdf.js");
const { sha256 } = require("@noble/hashes/sha256.js");
const { hmac } = require("@noble/hashes/hmac.js");
const { utf8ToBytes } = require("@noble/hashes/utils.js");

const MAGIC = [0x48, 0x53, 0x48, 0x44]; // "HSHD"
const VERSION = 2;
const MODE = 2;
const INFO = utf8ToBytes("hush-drop-v2");
const RENDEZVOUS_MSG = utf8ToBytes("hush-drop-rendezvous-v1");

/** Byte-identical layout to `seal_to_key` in crates/hush-core/src/drop.rs. */
export function sealToKey(pkg, recipientPk) {
  const ephSk = x25519.utils.randomPrivateKey();
  const ephPk = x25519.getPublicKey(ephSk);
  const shared = x25519.getSharedSecret(ephSk, recipientPk);
  const nonce = new Uint8Array(24);
  crypto.getRandomValues(nonce);

  const header = new Uint8Array(62);
  header.set(MAGIC, 0);
  header[4] = VERSION;
  header[5] = MODE;
  header.set(ephPk, 6);
  header.set(nonce, 38);

  const info = new Uint8Array(INFO.length + recipientPk.length);
  info.set(INFO, 0);
  info.set(recipientPk, INFO.length);
  const key = hkdf(sha256, shared, ephPk, info, 32);

  const cipher = xchacha20poly1305(key, nonce, header);
  const ct = cipher.encrypt(utf8ToBytes(JSON.stringify(pkg)));
  return { container: concat(header, ct), ephPk, key };
}

export function openWithKey(bytes, sk, pk) {
  if (bytes.length <= 62 || !MAGIC.every((b, i) => bytes[i] === b)) {
    throw new Error("BadDrop");
  }
  if (bytes[4] !== VERSION || bytes[5] !== MODE) throw new Error("UnsupportedDrop");
  const ephPk = bytes.slice(6, 38);
  const nonce = bytes.slice(38, 62);
  const shared = x25519.getSharedSecret(sk, ephPk);
  const info = new Uint8Array(INFO.length + pk.length);
  info.set(INFO, 0);
  info.set(pk, INFO.length);
  const key = hkdf(sha256, shared, ephPk, info, 32);
  const plain = xchacha20poly1305(key, nonce, bytes.slice(0, 62)).decrypt(
    bytes.slice(62),
  );
  return JSON.parse(new TextDecoder().decode(plain));
}

export function rendezvousId(recipientPk) {
  const mac = hmac(sha256, recipientPk, RENDEZVOUS_MSG);
  return toHex(mac.slice(0, 16));
}

function concat(a, b) {
  const out = new Uint8Array(a.length + b.length);
  out.set(a, 0);
  out.set(b, a.length);
  return out;
}

function toHex(bytes) {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

function fromHex(hex) {
  return new Uint8Array(hex.match(/.{2}/g).map((h) => parseInt(h, 16)));
}

const [mode, ...args] = process.argv.slice(2);

if (mode === "seal") {
  const prefix = args[0];
  if (!prefix) throw new Error("usage: drops-vector.mjs seal <fixture-prefix>");
  const sk = x25519.utils.randomPrivateKey();
  const pk = x25519.getPublicKey(sk);
  const pkg = {
    v: 2,
    subject: "from the page",
    body: "sealed by the noble stack, opened by hush-core",
    attachments: [{ name: "note.txt", data: Buffer.from("hi").toString("base64") }],
    created_unix: 1_760_000_000,
  };
  const { container } = sealToKey(pkg, pk);
  writeFileSync(`${prefix}.hshd`, container);
  writeFileSync(`${prefix}.sk`, `${toHex(sk)}\n`);
  writeFileSync(`${prefix}.pk`, `${toHex(pk)}\n`);

  // Self-check first: the noble stack must open what it sealed.
  const back = openWithKey(container, sk, pk);
  if (back.subject !== pkg.subject) throw new Error("JS round-trip failed");
  console.log(`fixture written: ${container.length} bytes, rendezvous ${rendezvousId(pk)}`);
} else if (mode === "open") {
  const [file, skHex, pkHex] = args;
  if (!file || !skHex || !pkHex) {
    throw new Error("usage: drops-vector.mjs open <container-file> <sk-hex> <pk-hex>");
  }
  const pkg = openWithKey(new Uint8Array(readFileSync(file)), fromHex(skHex), fromHex(pkHex));
  console.log(`opened: subject="${pkg.subject}" attachments=${pkg.attachments.length}`);
} else {
  throw new Error("usage: drops-vector.mjs seal <prefix> | open <file> <sk-hex> <pk-hex>");
}
