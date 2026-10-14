# Vendored Opus (fallback only)

`build.rs` links **system libopus** when `pkg-config` finds an `opus.pc` — the
proper dependency, and the only one a distro build should use. When there is
none (this development box cannot install `libopus-dev` non-interactively), it
compiles the vendored Opus 1.4 tree with `cc` instead.

The vendored tree is deliberately **not committed** — 75k lines of third-party
C does not belong in the repo. Fetch and extract it with:

```sh
curl -sL https://downloads.xiph.org/releases/opus/opus-1.4.tar.gz -o /tmp/opus-1.4.tar.gz
echo "c9b32b4253be5ae63d1ff16eea06b94b5f0f2951b7a02aceef58e3a3ce49c51f  /tmp/opus-1.4.tar.gz" | sha256sum -c
mkdir -p crates/hush-audio/vendor
tar xzf /tmp/opus-1.4.tar.gz -C crates/hush-audio/vendor \
  opus-1.4/include opus-1.4/src opus-1.4/celt opus-1.4/silk opus-1.4/COPYING
mv crates/hush-audio/vendor/opus-1.4 crates/hush-audio/vendor/opus
```

`build.rs` fails with a pointer here when there is no system libopus and the
tree is missing.
