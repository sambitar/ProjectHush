import { test } from "node:test";
import assert from "node:assert/strict";
import { markdownToHtml } from "./markdown.ts";

/**
 * Unit tests for the NIP-23 body renderer.
 *
 * A public post's body is written by a stranger, so most of what matters here is
 * what the renderer refuses to do: emit a tag the source wrote, fetch an image,
 * or turn a `javascript:` target into a link. The formatting cases are the part a
 * reader would notice; the refusals are the part that keeps the page safe.
 */

test("headings become headings at the level the source asked for", () => {
  assert.equal(markdownToHtml("# One"), "<h1>One</h1>");
  assert.equal(markdownToHtml("### Three"), "<h3>Three</h3>");
  // Seven hashes is not a heading: the marker runs out at six.
  assert.equal(markdownToHtml("####### Seven"), "<p>####### Seven</p>");
});

test("blank lines split paragraphs and single newlines do not", () => {
  assert.equal(
    markdownToHtml("one\ntwo\n\nthree"),
    "<p>one two</p>\n<p>three</p>",
  );
});

test("a fenced code block is escaped and not read as markup", () => {
  const html = markdownToHtml("```\n# not a heading\n<b>not bold</b>\n```");
  assert.equal(
    html,
    "<pre><code># not a heading\n&lt;b&gt;not bold&lt;/b&gt;</code></pre>",
  );
});

test("lists become lists, ordered when the marker is numbered", () => {
  assert.equal(
    markdownToHtml("- one\n- two"),
    "<ul><li>one</li><li>two</li></ul>",
  );
  assert.equal(
    markdownToHtml("1. one\n2. two"),
    "<ol><li>one</li><li>two</li></ol>",
  );
});

test("a quote becomes a quote, with its own blocks inside", () => {
  assert.equal(
    markdownToHtml("> one\n> two"),
    "<blockquote><p>one two</p></blockquote>",
  );
});

test("a rule becomes a rule, spaced markers included", () => {
  assert.equal(markdownToHtml("---"), "<hr>");
  assert.equal(markdownToHtml("***"), "<hr>");
  // `* * *` is a rule in Markdown. Read as a list item it would print as a blank
  // bullet, because its text `* *` reads as emphasis around a space.
  assert.equal(markdownToHtml("* * *"), "<hr>");
  assert.equal(markdownToHtml("--"), "<p>--</p>");
});

test("emphasis and code spans are applied, code first", () => {
  assert.equal(markdownToHtml("**bold** and *italic*"), "<p><strong>bold</strong> and <em>italic</em></p>");
  assert.equal(markdownToHtml("`a * b`"), "<p><code>a * b</code></p>");
});

test("raw HTML in the source is text, never markup", () => {
  // The whole point: a relay is not a trusted author.
  assert.equal(
    markdownToHtml("<script>alert(1)</script>"),
    "<p>&lt;script&gt;alert(1)&lt;/script&gt;</p>",
  );
  assert.equal(
    markdownToHtml('<img src=x onerror="alert(1)">'),
    "<p>&lt;img src=x onerror=&quot;alert(1)&quot;&gt;</p>",
  );
});

test("an image prints its alt text and never fetches", () => {
  // An image is a call-out to a stranger's server, so the page does not make it.
  assert.equal(
    markdownToHtml("![a tracking pixel](https://tracker.example/p.gif)"),
    "<p>a tracking pixel</p>",
  );
});

test("only http, https and mailto become links", () => {
  assert.equal(
    markdownToHtml("[site](https://example.com)"),
    '<p><a href="https://example.com" rel="noreferrer noopener">site</a></p>',
  );
  assert.equal(
    markdownToHtml("[mail](mailto:a@example.com)"),
    '<p><a href="mailto:a@example.com" rel="noreferrer noopener">mail</a></p>',
  );
  // A refused target keeps its label and loses its link.
  assert.equal(markdownToHtml("[click](javascript:alert(1))"), "<p>click</p>");
  assert.equal(markdownToHtml("[data](data:text/html;base64,PHNjcmlwdD4=)"), "<p>data</p>");
});

test("a link target is escaped inside the attribute", () => {
  assert.equal(
    markdownToHtml("[q](https://example.com/?a=1&b=2)"),
    '<p><a href="https://example.com/?a=1&amp;b=2" rel="noreferrer noopener">q</a></p>',
  );
});

test("an empty body is empty rather than an empty paragraph", () => {
  assert.equal(markdownToHtml(""), "");
  assert.equal(markdownToHtml("   \n\n  "), "");
});

test("a heading with a link is a heading, not a paragraph", () => {
  // Blocks and inlines compose: the block rule wins, the inline rule runs inside.
  assert.equal(
    markdownToHtml("## See [the site](https://example.com)"),
    '<h2>See <a href="https://example.com" rel="noreferrer noopener">the site</a></h2>',
  );
});
