import { test } from "node:test";
import assert from "node:assert/strict";
import {
  MAX_LINK_CHARS,
  normalizeExternalLink,
  parseLinkCommand,
  splitLinks,
} from "./links.ts";

test("a bare command is recognised so the caller can ask for a URL", () => {
  assert.deepEqual(parseLinkCommand("/link"), { url: "" });
  assert.deepEqual(parseLinkCommand("/link   "), { url: "" });
});

test("a URL is carried as typed", () => {
  assert.deepEqual(parseLinkCommand("/link https://example.com/a?b=1"), {
    url: "https://example.com/a?b=1",
  });
  assert.deepEqual(parseLinkCommand("  /link https://example.com  "), {
    url: "https://example.com",
  });
});

test("ordinary text and lookalikes are not commands", () => {
  assert.equal(parseLinkCommand("see /link https://example.com"), null);
  assert.equal(parseLinkCommand("/linked https://example.com"), null);
  assert.equal(parseLinkCommand("/link-me"), null);
  assert.equal(parseLinkCommand(""), null);
});

test("the keyword is case insensitive", () => {
  assert.deepEqual(parseLinkCommand("/Link https://example.com"), {
    url: "https://example.com",
  });
});

test("only absolute http(s) URLs without credentials are accepted", () => {
  assert.equal(normalizeExternalLink("https://example.com"), "https://example.com/");
  assert.equal(normalizeExternalLink("HTTP://EXAMPLE.COM"), "http://example.com/");
  assert.equal(normalizeExternalLink("javascript:alert(1)"), null);
  assert.equal(normalizeExternalLink("data:text/html,hi"), null);
  assert.equal(normalizeExternalLink("https://user:pass@example.com"), null);
  assert.equal(normalizeExternalLink("https://example.com/a b"), null);
  assert.equal(normalizeExternalLink(""), null);
  assert.equal(normalizeExternalLink(`https://example.com/${"a".repeat(MAX_LINK_CHARS)}`), null);
});

test("plain text is one text part", () => {
  assert.deepEqual(splitLinks("hello there"), [
    { kind: "text", text: "hello there" },
  ]);
  assert.deepEqual(splitLinks(""), []);
});

test("a URL becomes a link and trailing punctuation stays outside", () => {
  assert.deepEqual(splitLinks("See https://example.com."), [
    { kind: "text", text: "See " },
    {
      kind: "link",
      text: "https://example.com",
      href: "https://example.com/",
    },
    { kind: "text", text: "." },
  ]);
  assert.deepEqual(splitLinks("(https://example.com/a_(b))"), [
    { kind: "text", text: "(" },
    {
      kind: "link",
      text: "https://example.com/a_(b)",
      href: "https://example.com/a_(b)",
    },
    { kind: "text", text: ")" },
  ]);
});

test("a non-http or credential-bearing URL stays inert", () => {
  assert.deepEqual(splitLinks("javascript:alert(1)"), [
    { kind: "text", text: "javascript:alert(1)" },
  ]);
  assert.deepEqual(splitLinks("https://user:pass@example.com"), [
    { kind: "text", text: "https://user:pass@example.com" },
  ]);
});

test("two links keep the text between them", () => {
  assert.deepEqual(splitLinks("a https://one.example b https://two.example"), [
    { kind: "text", text: "a " },
    {
      kind: "link",
      text: "https://one.example",
      href: "https://one.example/",
    },
    { kind: "text", text: " b " },
    {
      kind: "link",
      text: "https://two.example",
      href: "https://two.example/",
    },
  ]);
});
