# Vendored Opus (spike-only)

The `voice-spike` crate compiles Opus 1.4 from source with `cc` (`build.rs`), because the engine
spike (Task 23 §2 #4) needed libopus and this environment could not install `libopus-dev`
non-interactively. The vendored tree is deliberately **not committed** — a throwaway spike does
not put 75k lines of third-party C into the repo.

Fetch and extract it with:

```sh
curl -sL https://downloads.xiph.org/releases/opus/opus-1.4.tar.gz -o /tmp/opus-1.4.tar.gz
echo "c9b32b4253be5ae63d1ff16eea06b94b5f0f2951b7a02aceef58e3a3ce49c51f  /tmp/opus-1.4.tar.gz" | sha256sum -c
mkdir -p crates/voice-spike/vendor
tar xzf /tmp/opus-1.4.tar.gz -C crates/voice-spike/vendor \
  opus-1.4/include opus-1.4/src opus-1.4/celt opus-1.4/silk opus-1.4/COPYING
mv crates/voice-spike/vendor/opus-1.4 crates/voice-spike/vendor/opus
```

`build.rs` fails with a pointer here when the tree is missing. The spike is evidence for the
§2 #4 decision (recorded in `docs/tasks/conclave-voice.md`); if the spike ever stops being one —
i.e. Phase 2 ships Opus in the daemon — the product replaces this vendor hack with a proper
dependency (system libopus with the dev package, or a maintained binding crate), never with this.
