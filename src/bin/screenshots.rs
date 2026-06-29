//! Render the app's screens over made-up data into the directory given
//! (`docs/static/screenshots` by default).
//!
//! `cargo run --release --features screenshots --bin screenshots`

#[allow(dead_code)]
#[path = "../gui.rs"]
mod gui;

fn main() -> Result<(), slint::PlatformError> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let out = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "docs/static/screenshots".into());
    gui::screenshots::run(&out)
}
