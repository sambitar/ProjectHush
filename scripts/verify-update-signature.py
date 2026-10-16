#!/usr/bin/env python3
"""Verify a released artifact against the public key compiled into the app.

The updater refuses any artifact whose signature does not match
`plugins.updater.pubkey` in `apps/hush-tauri/src-tauri/tauri.conf.json`, and it
refuses it on the user's machine, after a download, with no clue which end is
wrong. The failure it guards against is easy to cause and invisible until then:
rotate `TAURI_SIGNING_PRIVATE_KEY` without rebuilding with a matching pubkey, and
every future release signs fine and installs nowhere.

So this checks the pairing directly, against the artifacts as published:

    scripts/verify-update-signature.py \\
      --manifest https://hushrelay.space/updates/latest.json --all

    # or one artifact, already on disk
    scripts/verify-update-signature.py path/to/Hush_aarch64.app.tar.gz \\
      --sig path/to/Hush_aarch64.app.tar.gz.sig

Exit status is 0 only when every signature verifies. Requires the `cryptography`
package (`pip install cryptography`), which is why this is not part of the
publish path: that runs on jq, rsync and curl alone.

Minisign format notes, all established by testing against the real v0.1.0
release rather than from the spec:

- A release `.sig` file is the *base64* of a minisign signature file, not the
  minisign file itself. `latest.json`'s `signature` field is that base64 verbatim.
- The signature is Ed25519 over the artifact's BLAKE2b-512 digest when the
  algorithm bytes read "ED" (Tauri's tooling labels its output "ED"), and over the
  raw bytes when they read "Ed".
- The trailing global signature covers `signature || trusted_comment` with the
  literal `trusted comment: ` prefix stripped.
"""

import argparse
import base64
import hashlib
import json
import os
import sys
import urllib.request

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEFAULT_CONF = os.path.join(REPO_ROOT, "apps", "hush-tauri", "src-tauri", "tauri.conf.json")


def minisign_lines(blob_b64, what):
    """Decode a base64-wrapped minisign file into its non-empty lines."""
    try:
        text = base64.b64decode(blob_b64).decode()
    except Exception as exc:
        raise SystemExit(f"error: {what} is not base64: {exc}")
    lines = [l for l in text.splitlines() if l.strip()]
    if len(lines) < 2:
        raise SystemExit(f"error: {what} does not look like a minisign file")
    return lines


def load_pubkey(conf_path):
    with open(conf_path, "r", encoding="utf-8") as fh:
        conf = json.load(fh)
    try:
        blob = conf["plugins"]["updater"]["pubkey"]
    except KeyError:
        raise SystemExit(f"error: {conf_path} has no plugins.updater.pubkey")
    raw = base64.b64decode(minisign_lines(blob, "the pubkey")[1])
    if len(raw) != 42:
        raise SystemExit(f"error: pubkey is {len(raw)} bytes, expected 42")
    return raw[2:10], raw[10:42]


def parse_signature(sig_text):
    lines = minisign_lines(sig_text.strip(), "the signature")
    raw = base64.b64decode(lines[1])
    if len(raw) != 74:
        raise SystemExit(f"error: signature is {len(raw)} bytes, expected 74")
    trusted = lines[2]
    if trusted.startswith("trusted comment: "):
        trusted = trusted[len("trusted comment: "):]
    if len(lines) < 4:
        return raw[:2], raw[2:10], raw[10:74], None, None
    return raw[:2], raw[2:10], raw[10:74], trusted.encode(), base64.b64decode(lines[3])


def fetch(url):
    with urllib.request.urlopen(url, timeout=120) as resp:
        return resp.read()


def verify(artifact_bytes, sig_text, key_id, pubkey, label):
    sig_alg, sig_key_id, sig, trusted, global_sig = parse_signature(sig_text)
    if sig_key_id != key_id:
        print(f"FAIL {label}: signed by key {sig_key_id.hex()}, the app carries {key_id.hex()}")
        return False
    key = Ed25519PublicKey.from_public_bytes(pubkey)
    message = hashlib.blake2b(artifact_bytes, digest_size=64).digest() if sig_alg == b"ED" else artifact_bytes
    try:
        key.verify(sig, message)
        if trusted and global_sig:
            key.verify(global_sig, sig + trusted)
    except Exception:
        print(f"FAIL {label}: signature does not verify against the app's public key")
        return False
    print(f"OK   {label}: verifies against the app's public key")
    return True


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("artifact", nargs="?", help="artifact path or URL to verify")
    ap.add_argument("--sig", help="signature path; defaults to <artifact>.sig, or to the manifest entry")
    ap.add_argument("--manifest", help="latest.json path or URL; verifies every platform entry")
    ap.add_argument("--all", action="store_true", help="with --manifest, verify every platform (downloads each)")
    ap.add_argument("--conf", default=DEFAULT_CONF, help="tauri.conf.json to read the pubkey from")
    args = ap.parse_args()

    if not args.artifact and not args.manifest:
        ap.error("give an artifact, or --manifest")

    key_id, pubkey = load_pubkey(args.conf)
    print(f"public key {key_id.hex()} from {os.path.relpath(args.conf, REPO_ROOT)}")

    if args.manifest:
        manifest_text = fetch(args.manifest) if args.manifest.startswith("http") else open(args.manifest, "rb").read()
        manifest = json.loads(manifest_text)
        platforms = manifest.get("platforms", {})
        if not platforms:
            raise SystemExit("error: the manifest lists no platforms")
        print(f"manifest {args.manifest}: version {manifest.get('version')}, {len(platforms)} platform entries")
        targets = sorted(platforms.items()) if args.all else sorted(platforms.items())[:1]
        ok = True
        for name, entry in targets:
            url, signature = entry.get("url"), entry.get("signature")
            if not url or not signature:
                print(f"FAIL {name}: the manifest entry has no url or signature")
                ok = False
                continue
            ok = verify(fetch(url), signature, key_id, pubkey, name) and ok
        if not args.all and len(platforms) > 1:
            print("(--all would check the remaining entries; each one downloads its artifact)")
        return 0 if ok else 1

    if args.artifact.startswith("http"):
        data = fetch(args.artifact)
    else:
        with open(args.artifact, "rb") as fh:
            data = fh.read()
    if args.sig:
        sig_text = open(args.sig, "r", encoding="utf-8").read()
    elif args.artifact.startswith("http"):
        sig_text = fetch(args.artifact + ".sig").decode()
    else:
        sig_text = open(args.artifact + ".sig", "r", encoding="utf-8").read()
    return 0 if verify(data, sig_text, key_id, pubkey, os.path.basename(args.artifact)) else 1


if __name__ == "__main__":
    sys.exit(main())
