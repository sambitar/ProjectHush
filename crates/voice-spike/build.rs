//! Build the vendored Opus 1.4 with `cc`, float build, no x86 intrinsics —
//! the plain-C paths, exactly what a system libopus would supply without
//! needing `libopus-dev` (which this box cannot install non-interactively).

fn main() {
    if !std::path::Path::new("vendor/opus/include/opus.h").exists() {
        panic!(
            "voice-spike needs the vendored Opus 1.4 tree: see \
             crates/voice-spike/vendor/README.md for the pinned fetch"
        );
    }
    let mut build = cc::Build::new();
    build
        .define("OPUS_BUILD", None)
        .define("HAVE_LRINT", None)
        .define("HAVE_LRINTF", None)
        // The temporary-allocation mode: C99 variable-length arrays, the
        // standard choice on Linux.
        .define("VAR_ARRAYS", None)
        .include("vendor/opus/include")
        .include("vendor/opus/src")
        .include("vendor/opus/celt")
        .include("vendor/opus/silk")
        .include("vendor/opus/silk/float")
        .include("vendor/opus/silk/fixed")
        .warnings(false)
        .opt_level(2);

    let skip: &[&str] = &[
        "opus_compare.c",
        "opus_demo.c",
        "repacketizer_demo.c",
        "opus_custom_demo.c",
    ];

    for dir in ["src", "celt", "silk", "silk/float"] {
        let pattern = format!("vendor/opus/{dir}/*.c");
        for path in glob_paths(&pattern) {
            let name = path.rsplit('/').next().unwrap_or("");
            if skip.contains(&name) {
                continue;
            }
            build.file(&path);
            println!("cargo:rerun-if-changed={path}");
        }
    }

    build.compile("opus");
}

/// `glob`-less glob: one directory level of `*.c`, which is all the vendor
/// tree needs and keeps `cc` as the only build dependency.
fn glob_paths(pattern: &str) -> Vec<String> {
    let dir = match pattern.rfind('/') {
        Some(at) => &pattern[..at],
        None => ".",
    };
    let mut found = Vec::new();
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {dir}: {e}"));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.ends_with(".c") {
                found.push(path.display().to_string());
            }
        }
    }
    found.sort();
    found
}
