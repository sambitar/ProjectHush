import { test } from "node:test";
import assert from "node:assert/strict";
import { parseTorrentCommand } from "./torrentCommand.ts";

test("bare /torrent is a command", () => {
  assert.deepEqual(parseTorrentCommand("/torrent"), { arg: "" });
  assert.deepEqual(parseTorrentCommand("/torrent   "), { arg: "" });
});

test("an argument is carried, but the picker still decides", () => {
  assert.deepEqual(parseTorrentCommand("/torrent ubuntu"), { arg: "ubuntu" });
  assert.deepEqual(parseTorrentCommand("  /TORRENT film  "), { arg: "film" });
});

test("ordinary text and lookalikes are not commands", () => {
  assert.equal(parseTorrentCommand("send /torrent"), null);
  assert.equal(parseTorrentCommand("/torrents"), null);
  assert.equal(parseTorrentCommand("/torrent-me"), null);
  assert.equal(parseTorrentCommand(""), null);
});
