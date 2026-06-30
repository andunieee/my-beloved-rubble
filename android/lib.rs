//! The Android app: the same GUI as the desktop (`src/gui.rs`), started
//! by the native activity. The app keeps its state and its folders in its
//! own storage, which (unlike shared storage) can hold the folders' links.

#[allow(dead_code)]
#[path = "../src/gui.rs"]
mod gui;

use slint::android::AndroidApp;

#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("mbr"),
    );
    if let Some(dir) = app.internal_data_path() {
        let tmp = dir.join("tmp");
        std::fs::create_dir_all(&tmp).ok();
        // Before any other thread starts: the default, /data/local/tmp,
        // isn't writable by apps.
        unsafe { std::env::set_var("TMPDIR", &tmp) };
        mbr::state::AppState::use_app_dir(dir);
    }
    gui::use_activity(&app);
    if let Err(e) = slint::android::init(app) {
        log::error!("cannot start: {e}");
        return;
    }
    if let Err(e) = gui::run() {
        log::error!("{e}");
    }
}
