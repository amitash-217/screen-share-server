fn main() {
    slint_build::compile("ui/app.slint").expect("compile slint ui");

    // libwebrtc's Objective-C categories are dropped by the linker without -ObjC, and a
    // dependency's own `rustc-link-arg` does not reach our binaries and tests. Without
    // this flag the app crashes at start with `+[NSString stringForStdString:]`.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-ObjC");
        // Same for system audio capture: ScreenCaptureKit's bindings need the Swift runtime.
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}
