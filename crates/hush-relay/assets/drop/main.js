// The drop page's logic: seal a message to a published drop key, upload once.
//
// Runs in whatever browser the dropper brings — Tor Browser first. Everything
// happens here, client-side: the relay never sees the plaintext, the drop
// key's private half, or anything that identifies the dropper. One PUT, no
// cookies, no storage, no external requests.
//
// Byte layout reference: crates/hush-core/src/drop.rs (hush-drop-v2 sealed
// box) and scripts/drops-vector.mjs, which proves this stack and the Rust one
// agree.

import { x25519 } from "@noble/curves/ed25519";
import { xchacha20poly1305 } from "@noble/ciphers/chacha";
import { hkdf } from "@noble/hashes/hkdf";
import { sha256 } from "@noble/hashes/sha256";
import { hmac } from "@noble/hashes/hmac";
import { utf8ToBytes } from "@noble/hashes/utils.js";

const MAGIC = [0x48, 0x53, 0x48, 0x44]; // "HSHD"
const VERSION = 2;
const MODE = 2;
const HEADER_LEN = 62;
const MAX_DROP_BYTES = 8 * 1024 * 1024;
const KEY_PREFIX = "hush-drop-pk-v1:";
const INFO = utf8ToBytes("hush-drop-v2");
const RENDEZVOUS_MSG = utf8ToBytes("hush-drop-rendezvous-v1");

const el = (id) => document.getElementById(id);
const keyInput = el("key");
const subjectInput = el("subject");
const bodyInput = el("body");
const fileInput = el("files");
const sizeLine = el("size-line");
const sendBtn = el("send");
const statusLine = el("status");
const resultBox = el("result");
const resultLink = el("result-link");

function say(text) {
  statusLine.textContent = text;
}

/** Decode the pasted key or return null — the prefix is the wrong-half guard. */
function parseKey() {
  const pasted = keyInput.value.trim();
  if (!pasted.startsWith(KEY_PREFIX)) return null;
  let raw;
  try {
    const b64 = pasted.slice(KEY_PREFIX.length);
    raw = Uint8Array.from(atob(b64.replace(/-/g, "+").replace(/_/g, "/")), (c) => c.charCodeAt(0));
  } catch {
    return null;
  }
  return raw.length === 32 ? raw : null;
}

function readAsBytes(file) {
  return file.arrayBuffer().then((buf) => new Uint8Array(buf));
}

function toBase64(bytes) {
  let binary = "";
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return btoa(binary);
}

/** The delivery id both the page and the member's daemon derive from the key. */
function rendezvousId(recipientPk) {
  const mac = hmac(sha256, recipientPk, RENDEZVOUS_MSG);
  return toHex(mac.slice(0, 16));
}

function toHex(bytes) {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

async function gatherAttachments() {
  const files = Array.from(fileInput.files);
  const attachments = [];
  let total = 0;
  for (const file of files) {
    const bytes = await readAsBytes(file);
    total += bytes.length;
    attachments.push({ name: file.name, data: toBase64(bytes) });
  }
  return { attachments, total };
}

function meter(text, ok) {
  sizeLine.textContent = text;
  sizeLine.classList.toggle("over", !ok);
}

async function updateMeter() {
  const key = parseKey();
  if (!key) {
    meter("Paste a valid drop key (starts with " + KEY_PREFIX + ").", false);
    return;
  }
  const { attachments, total } = await gatherAttachments();
  const pkg = {
    v: 2,
    subject: subjectInput.value,
    body: bodyInput.value,
    attachments,
    created_unix: Math.floor(Date.now() / 1000),
  };
  // The container is the header plus the sealed JSON; base64 inflates files by
  // a third before that, so meter the serialized bytes, not the raw files.
  const jsonBytes = utf8ToBytes(JSON.stringify(pkg)).length;
  const containerBytes = HEADER_LEN + jsonBytes + 16;
  const ok = containerBytes <= MAX_DROP_BYTES;
  const limit = (MAX_DROP_BYTES / (1024 * 1024)).toFixed(0);
  meter(
    (containerBytes / 1024).toFixed(1) + " KiB sealed of " + limit + " MiB" +
      (ok ? "" : " — too large, remove some files"),
    ok,
  );
  return { pkg, key, ok };
}

async function sealAndUpload() {
  const built = await updateMeter();
  if (!built || !built.ok) {
    say("Fix the size or the key first.");
    return;
  }
  const { pkg, key } = built;
  if (!bodyInput.value.trim() && !fileInput.files.length) {
    say("Write something or attach a file — an empty drop helps nobody.");
    return;
  }

  sendBtn.disabled = true;
  resultBox.hidden = true;
  say("Sealing…");

  try {
    const ephSk = x25519.utils.randomPrivateKey();
    const ephPk = x25519.getPublicKey(ephSk);
    const shared = x25519.getSharedSecret(ephSk, key);
    const nonce = new Uint8Array(24);
    crypto.getRandomValues(nonce);

    const header = new Uint8Array(HEADER_LEN);
    header.set(MAGIC, 0);
    header[4] = VERSION;
    header[5] = MODE;
    header.set(ephPk, 6);
    header.set(nonce, 38);

    const info = new Uint8Array(INFO.length + key.length);
    info.set(INFO, 0);
    info.set(key, INFO.length);
    const keyBytes = hkdf(sha256, shared, ephPk, info, 32);

    const cipher = xchacha20poly1305(keyBytes, nonce, header);
    const ciphertext = cipher.encrypt(utf8ToBytes(JSON.stringify(pkg)));
    const container = new Uint8Array(HEADER_LEN + ciphertext.length);
    container.set(header, 0);
    container.set(ciphertext, HEADER_LEN);

    say("Uploading…");
    const id = rendezvousId(key);
    // Relative on purpose: the page works identically on the relay's clearnet
    // and onion bases, and never names another origin.
    const res = await fetch("drop/" + id, { method: "PUT", body: container });
    if (res.status !== 204) {
      throw new Error("the server refused the upload (HTTP " + res.status + ")");
    }
    say("");
    resultBox.hidden = false;
    resultLink.textContent = "Delivered. The recipient will find it on their next check.";
  } catch (err) {
    say("Could not deliver: " + err.message);
  } finally {
    sendBtn.disabled = false;
  }
}

keyInput.addEventListener("input", () => void updateMeter());
fileInput.addEventListener("change", () => void updateMeter());
bodyInput.addEventListener("input", () => void updateMeter());
sendBtn.addEventListener("click", sealAndUpload);
updateMeter();
