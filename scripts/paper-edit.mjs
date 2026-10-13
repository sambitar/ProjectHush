#!/usr/bin/env node
// Paper dogfood helper: build and read the Yjs updates the Paper editor sends,
// so a harness can drive /api/paper/* with edits of the shape the editor really
// produces rather than a toy one.
//
// The webview binds ProseMirror through `y-prosemirror` to the XML fragment
// named "prosemirror" (apps/hush-tauri/src/lib/paperStore.ts), so an edit is an
// XmlFragment of `doc`/`paragraph` nodes holding XmlText. The daemon treats
// updates as opaque bytes by design (paper_doc.rs never parses content), so a
// plain Y.Text would exercise the transport equally well — but a dogfood that
// uses a different shape than the editor would not notice if the two ever
// diverged, which is the point of running it.
//
// `yjs` is resolved from the UI's own node_modules so the bytes come from the
// same build the editor ships.
//
//   paper-edit.mjs new <text>              base64 v1 update: one paragraph
//   paper-edit.mjs append <state> <text>   base64 v1 update: state + a paragraph
//   paper-edit.mjs read <state>            print the text the state holds
//   paper-edit.mjs presence <name> <color> base64 awareness frame: a cursor
//   paper-edit.mjs cursor <frame>          print the name and cursor in a frame
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

const UI_DIR = fileURLToPath(new URL("../apps/hush-tauri/", import.meta.url));
const require = createRequire(`${UI_DIR}package.json`);
const Y = require("yjs");
const {
  Awareness,
  applyAwarenessUpdate,
  encodeAwarenessUpdate,
} = require("y-protocols/awareness");

/// The fragment name the editor binds to; see paperStore.ts.
const FRAGMENT_NAME = "prosemirror";

const b64ToBytes = (b64) => new Uint8Array(Buffer.from(b64, "base64"));
const bytesToB64 = (bytes) => Buffer.from(bytes).toString("base64");

/// A doc seeded from a previously produced state, or empty when there is none.
function loadDoc(state) {
  const doc = new Y.Doc();
  if (state) {
    Y.applyUpdate(doc, b64ToBytes(state));
  }
  return doc;
}

/// The fragment's `<doc>` element, created when the paper is still empty.
///
/// y-prosemirror encodes the ProseMirror document as one `doc` element in the
/// fragment, so an edit appends a paragraph inside it rather than to the
/// fragment directly — the shape the editor can load back.
function docElement(doc) {
  const fragment = doc.getXmlFragment(FRAGMENT_NAME);
  const found = fragment
    .toArray()
    .find((node) => node instanceof Y.XmlElement && node.nodeName === "doc");
  if (found) {
    return found;
  }
  const element = new Y.XmlElement("doc");
  fragment.insert(fragment.length, [element]);
  return element;
}

/// Append one paragraph holding `text`.
function addParagraph(doc, text) {
  const root = docElement(doc);
  const element = new Y.XmlElement("paragraph");
  root.insert(root.length, [element]);
  element.insert(0, [text]);
}

/// Flatten a fragment to its text so the harness can assert on content.
///
/// `toArray()` yields plain strings for text runs, not `XmlText` instances, so
/// the string case is the one that actually carries the words. The top-level
/// value is an `XmlFragment`, and `XmlElement` extends it, so one branch
/// covers the whole tree.
function textOf(node) {
  if (typeof node === "string") {
    return node;
  }
  if (node instanceof Y.XmlText) {
    return node.toString();
  }
  if (node instanceof Y.XmlFragment) {
    return node.toArray().map(textOf).join("");
  }
  return "";
}

const [command, ...args] = process.argv.slice(2);

switch (command) {
  case "new": {
    const doc = new Y.Doc();
    addParagraph(doc, args[0] ?? "");
    process.stdout.write(bytesToB64(Y.encodeStateAsUpdate(doc)));
    break;
  }
  case "append": {
    const doc = loadDoc(args[0]);
    addParagraph(doc, args[1] ?? "");
    process.stdout.write(bytesToB64(Y.encodeStateAsUpdate(doc)));
    break;
  }
  case "read": {
    const doc = loadDoc(args[0]);
    process.stdout.write(textOf(doc.getXmlFragment(FRAGMENT_NAME)));
    break;
  }
  // Presence is the editor's awareness update — cursor as a Yjs relative
  // position, plus the author's name and colour — and the daemon treats it as
  // opaque bytes. This builds one from a real `y-prosemirror`-shaped doc so the
  // dogfood moves the same bytes a second editor window would.
  case "presence": {
    const doc = new Y.Doc();
    addParagraph(doc, "the paper");
    const awareness = new Awareness(doc);
    awareness.setLocalState({
      user: { name: args[0] ?? "someone", color: args[1] ?? "#2b6cb0" },
      cursor: {
        anchor: Y.createRelativePositionFromTypeIndex(
          doc.getXmlFragment(FRAGMENT_NAME),
          0,
        ),
        head: Y.createRelativePositionFromTypeIndex(
          doc.getXmlFragment(FRAGMENT_NAME),
          0,
        ),
      },
    });
    process.stdout.write(
      bytesToB64(encodeAwarenessUpdate(awareness, [awareness.clientID])),
    );
    awareness.destroy();
    break;
  }
  case "cursor": {
    const doc = new Y.Doc();
    const awareness = new Awareness(doc);
    applyAwarenessUpdate(awareness, b64ToBytes(args[0]), "remote");
    // The receiver's own awareness carries an empty state for its own client id,
    // so the peer's is the one that names a user.
    const state =
      [...awareness.getStates().values()].find((s) => s?.user?.name) ?? {};
    process.stdout.write(`${state.user?.name ?? ""}|${state.user?.color ?? ""}`);
    awareness.destroy();
    break;
  }
  default:
    process.stderr.write(
      "usage: paper-edit.mjs new <text> | append <state-b64> <text> | " +
        "read <state-b64> | presence <name> <color> | cursor <frame-b64>\n",
    );
    process.exit(2);
}
