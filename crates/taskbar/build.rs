fn main() {
    // `.shared` holds the config written by the app and read by every process
    // that loaded this DLL (same file => same physical pages).
    println!("cargo:rustc-cdylib-link-arg=/SECTION:.shared,RWS");
}
