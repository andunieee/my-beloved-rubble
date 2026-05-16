//! Remote setup against the embedded rclone, using a throwaway config file.

use mbr::rclone::{self, SetupOutcome};
use serde_json::json;

#[test]
fn setup_never_leaves_half_configured_remotes() {
    let config = std::env::temp_dir().join(format!("mbr-test-rclone-{}.conf", std::process::id()));
    std::fs::write(&config, "").unwrap();

    rclone::with_config(&config, || {
        // A clean setup completes and leaves the remote in place.
        let outcome = rclone::begin_setup("disk", "local", json!({}))?;
        assert!(matches!(outcome, SetupOutcome::Done), "{outcome:?}");
        assert!(rclone::remote_exists("disk")?);

        // Setting it up again is refused and does not delete it.
        assert!(rclone::begin_setup("disk", "local", json!({})).is_err());
        assert!(rclone::remote_exists("disk")?);

        // Form answers land in the config without further questions.
        let outcome = rclone::begin_setup(
            "dav",
            "webdav",
            json!({"url": "https://example.com/dav", "vendor": "nextcloud"}),
        )?;
        assert!(matches!(outcome, SetupOutcome::Done), "{outcome:?}");
        let stored = rclone::call("config/get", json!({"name": "dav"}))?;
        assert_eq!(stored["vendor"], "nextcloud");
        assert_eq!(stored["url"], "https://example.com/dav");

        // OAuth backends still get their sign-in questions; cancelling one
        // removes the half-configured remote.
        let SetupOutcome::Question(question) = rclone::begin_setup("gd", "drive", json!({}))? else {
            panic!("drive setup asked no OAuth question");
        };
        assert!(rclone::remote_exists("gd")?);
        rclone::cancel_setup("gd", &question.state).ok();
        assert!(!rclone::remote_exists("gd")?);

        // A failing setup removes whatever it created.
        assert!(rclone::begin_setup("broken", "no-such-backend", json!({})).is_err());
        assert!(!rclone::remote_exists("broken")?);
        Ok(())
    })
    .unwrap();

    std::fs::remove_file(&config).ok();
}
