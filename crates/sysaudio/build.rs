fn main() {
    // ScreenCaptureKit's bindings need the Swift runtime, and a dependency's `rustc-link-arg`
    // does not reach this crate's test binary. Without it the binary aborts at start with
    // `Library not loaded: @rpath/libswift_Concurrency.dylib`.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}
