use std::path::{Path, PathBuf};

const PROTOC_HELP: &str = "\
protoc not found.

The Signal session code (libsignal-rust) compiles protos/WhisperTextProtocol.proto
from its own build script, so a protoc binary is required before anything that
depends on hush-core can build. Install one of:

  Debian/Ubuntu     sudo apt install protobuf-compiler
  Fedora            sudo dnf install protobuf-compiler
  macOS             brew install protobuf
  any platform      unzip a protoc release into .tools/protoc-dist/
  already have one  PROTOC=/path/to/protoc cargo build
";

// protoc is a hard build requirement, and this is the earliest place a plain
// `cargo build` can say so in words that name it. Without this check the failure
// arrives as a prost-build error from libsignal-rust, a crate nobody typed,
// which never mentions protoc or where to get one.
//
// The check cannot fix the problem. Cargo runs each build script in its own
// process, so PROTOC set here would reach this process and nothing else — in
// particular not libsignal's build script. Only the environment the caller gives
// to cargo reaches that one, which is why scripts/lib/prereqs.sh exports it and
// why `PROTOC=... cargo build` works by hand.
fn main() {
    if resolve_protoc().is_none() {
        panic!("{}", PROTOC_HELP);
    }
}

fn resolve_protoc() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("PROTOC") {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Some(path);
        }
        eprintln!(
            "PROTOC={} is not a file — looking for another protoc.",
            path.display()
        );
    }
    // The vendored copy, when a protoc release has been unzipped there. `.tools/`
    // is not in the repository, so this is the developer-machine case.
    let vendored = Path::new("../../.tools/protoc-dist/bin/protoc");
    if vendored.is_file() {
        return Some(
            vendored
                .canonicalize()
                .unwrap_or_else(|_| vendored.to_path_buf()),
        );
    }
    // Whatever is on PATH, which is how prost-build itself would find it.
    let binary = if cfg!(windows) { "protoc.exe" } else { "protoc" };
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(binary))
            .find(|candidate| candidate.is_file())
    })
}
