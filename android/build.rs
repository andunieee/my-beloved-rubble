fn main() {
    slint_build::compile("../ui/app.slint").unwrap();
    // rclone, built as a shared library by go-shim/go (see the android-build recipe in the justfile).
    if let Ok(dir) = std::env::var("RCLONE_LIB_DIR") {
        println!("cargo:rerun-if-env-changed=RCLONE_LIB_DIR");
        println!("cargo:rustc-link-search=native={dir}");
        println!("cargo:rustc-link-lib=dylib=rclonego");
    }
}
