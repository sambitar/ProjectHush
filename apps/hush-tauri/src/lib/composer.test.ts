import { test } from "node:test";
import assert from "node:assert/strict";
import {
  composerEnabled,
  composerHint,
  composerPlaceholder,
  composerState,
} from "./composer.ts";

test("a mail-only trust has no live box, whatever its presence says", () => {
  assert.equal(composerState(true, false), "mail_only");
  assert.equal(composerState(false, false), "mail_only");
});

test("the box is online only when the peer is, and offline otherwise", () => {
  assert.equal(composerState(true, true), "online");
  assert.equal(composerState(false, true), "offline");
});

test("the hint is the mail-only reason, the offline one, or nothing", () => {
  // Pinned exactly: this copy is drawn by the builder and rewritten by the
  // patch, so a change here is a change to both.
  assert.equal(
    composerHint("Ana", "mail_only"),
    "Ana is mail only — chat and conclaves are off for this trust. Use Mail, or change this in their menu in Trusts.",
  );
  assert.equal(
    composerHint("Ana", "offline"),
    "Ana is offline. Chat and files need you both online — use Mail to reach them later.",
  );
  assert.equal(composerHint("Ana", "online"), null);
});

test("the hint names the peer by the name the caller shows", () => {
  // A local rename is drawn text: the box must not call a contact by an id.
  assert.match(composerHint("Ana B", "offline") ?? "", /^Ana B is offline\./);
});

test("the placeholder and the enabled flag follow the state", () => {
  assert.equal(composerPlaceholder("online"), "Message");
  assert.equal(
    composerPlaceholder("offline"),
    "Waiting for them to come online…",
  );
  assert.equal(composerEnabled("online"), true);
  assert.equal(composerEnabled("offline"), false);
  assert.equal(composerEnabled("mail_only"), false);
});
