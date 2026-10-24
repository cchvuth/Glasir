fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    // Windows: the .exe's own icon (Explorer, pinned taskbar); the window icon is set at runtime.
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new().set_icon("assets/icon.ico").compile().expect("embed the Windows icon");
    }
}
