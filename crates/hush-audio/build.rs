//! Link libopus two ways, preferring the proper dependency (the spike's
//! `vendor/README.md` rule: the product links system libopus where the distro
//! provides it) and falling back to the vendored Opus 1.4 compiled with `cc`
//! when there is no `opus.pc` — as on this development box, where the dev
//! package cannot be installed non-interactively. The vendored tree is not
//! committed; `vendor/README.md` carries the pinned fetch.

fn main() {
    // A system libopus wins: one shared C dependency, distro-audited, no
    // vendored source to explain.
    if pkg_config::probe_library("opus").is_ok() {
        return;
    }

    if !std::path::Path::new("vendor/opus/include/opus.h").exists() {
        panic!(
            "hush-audio found no system libopus (pkg-config `opus`) and no \
             vendored tree: see crates/hush-audio/vendor/README.md for the \
             pinned fetch"
        );
    }

    let mut build = cc::Build::new();
    build
        .define("OPUS_BUILD", None)
        .define("HAVE_LRINT", None)
        .define("HAVE_LRINTF", None)
        .include("vendor/opus/include")
        .include("vendor/opus/src")
        .include("vendor/opus/celt")
        .include("vendor/opus/silk")
        .include("vendor/opus/silk/float")
        .include("vendor/opus/silk/fixed")
        .warnings(false)
        .opt_level(2);

    // The temporary-allocation mode, which Opus's `celt/stack_alloc.h` selects
    // between. C99 variable-length arrays are the standard choice on Linux and
    // macOS. MSVC has no VLAs at all and rejects every one of them with
    // C2057/C2466/C2133 — `analysis.c` and `opus_decoder.c` are where it starts
    // — so the alloca mode is used there instead, which is the mode Opus's own
    // CMake build picks for Visual Studio. Both are one macro, so the host
    // platforms keep the mode they already had.
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_env == "msvc" {
        build.define("USE_ALLOCA", None);
    } else {
        build.define("VAR_ARRAYS", None);
    }

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
