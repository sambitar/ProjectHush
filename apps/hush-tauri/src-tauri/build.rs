fn main() {
    // Android's audio stack (oboe, through rodio/cpal) is C++; Rust links it in
    // but does not ask for the C++ runtime by name, leaving `__cxa_*` symbols
    // undefined in the cdylib. Android's loader resolves those only from a
    // library the app actually ships, so name it here and package
    // `libc++_shared.so` beside the cdylib in Gradle
    // (`gen/android/app/build.gradle.kts`).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
        println!("cargo:rustc-link-lib=c++_shared");
    }
    tauri_build::build()
}
