//! Cross-checks the hardcoded backend registry in `mbr::backends` against
//! the metadata of the embedded rclone itself (`config/providers`), so a
//! renamed or removed config key upstream breaks the build here instead of
//! producing a form rclone silently ignores.
//!
//! Runs as an integration test (`cargo test`) since librclone keeps
//! process-global state; the RPC lock serializes the calls.

use mbr::backends::{Kind, BACKENDS};
use serde_json::Value;

/// Fetch the `Options` array for one backend from `config/providers`.
fn provider_options(backend: &str) -> Result<Vec<Value>, String> {
    let providers = mbr::rclone::call("config/providers", serde_json::json!({}))?;
    let providers = providers
        .get("providers")
        .and_then(Value::as_array)
        .ok_or_else(|| "config/providers response has no providers array".to_owned())?;
    let provider = providers
        .iter()
        // `Prefix` is the command-line type name (e.g. `googlecloudstorage`);
        // `Name` is the display name it usually equals, lowercased.
        .find(|p| {
            p.get("Prefix").and_then(Value::as_str) == Some(backend)
                || p.get("Name").and_then(Value::as_str).map(str::to_lowercase).as_deref() == Some(backend)
        })
        .ok_or_else(|| format!("rclone knows no backend '{backend}'"))?;
    Ok(provider
        .get("Options")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

fn option_field<'a>(option: &'a Value, name: &str) -> Option<&'a Value> {
    option.get(name)
}

#[test]
/// Every non-target field of the registry must be a real, non-hidden,
/// non-advanced config option of the embedded rclone backend, with
/// secrets flagged `IsPassword` and choice examples offered by rclone.
fn registry_matches_rclone_providers() {
    for backend in BACKENDS {
        let options = provider_options(backend.name)
            .unwrap_or_else(|e| panic!("backend {}: {e}", backend.name));

        for field in backend.fields {
            if field.to_target {
                // Consumed into the mbr target; nothing to validate.
                continue;
            }
            let option = options
                .iter()
                .find(|o| o.get("Name").and_then(Value::as_str) == Some(field.name))
                .unwrap_or_else(|| {
                    panic!(
                        "backend '{}': registry field '{}' is not a config option of this rclone",
                        backend.name, field.name
                    )
                });

            if option_field(option, "Advanced").and_then(Value::as_bool) == Some(true) {
                panic!(
                    "backend '{}': field '{}' is advanced upstream; it should not be on the main form",
                    backend.name, field.name
                );
            }
            if option_field(option, "Hide").and_then(Value::as_bool) == Some(true) {
                panic!(
                    "backend '{}': field '{}' is hidden upstream",
                    backend.name, field.name
                );
            }

            let is_password = option_field(option, "IsPassword")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let sensitive = option_field(option, "Sensitive")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let upstream_required = option_field(option, "Required")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            assert_eq!(
                field.required, upstream_required,
                "backend '{}': required flag of field '{}' disagrees with upstream",
                backend.name, field.name
            );
            match field.kind {
                Kind::Secret => assert!(
                    is_password || sensitive,
                    "backend '{}': field '{}' is not a secret upstream",
                    backend.name,
                    field.name
                ),
                Kind::Choice => {
                    let examples: Vec<String> = option
                        .get("Examples")
                        .and_then(Value::as_array)
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|item| {
                                    item.get("Value")
                                        .and_then(Value::as_str)
                                        .map(str::to_owned)
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    assert!(
                        !examples.is_empty(),
                        "backend '{}': field '{}' has no examples upstream",
                        backend.name,
                        field.name
                    );
                    for allowed in field.examples {
                        assert!(
                            examples.iter().any(|e| e == allowed),
                            "backend '{}': example '{allowed}' of field '{}' not offered upstream (offers: {examples:?})",
                            backend.name,
                            field.name
                        );
                    }
                }
                Kind::Text => {}
            }
        }
    }
}

#[test]
/// The webdav vendor examples must match the embedded rclone exactly, so
/// the GUI dropdown offers the same set rclone would.
fn webdav_vendor_list_matches_rclone() {
    let options = provider_options("webdav").unwrap();
    let vendor = options
        .iter()
        .find(|o| o.get("Name").and_then(Value::as_str) == Some("vendor"))
        .expect("webdav has a vendor option");
    let mut upstream: Vec<String> = vendor
        .get("Examples")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("Value").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let mut ours: Vec<String> = BACKENDS
        .iter()
        .find(|b| b.name == "webdav")
        .unwrap()
        .fields
        .iter()
        .find(|f| f.name == "vendor")
        .unwrap()
        .examples
        .iter()
        .map(|e| e.to_string())
        .collect();
    upstream.sort();
    ours.sort();
    assert_eq!(ours, upstream);
}

#[test]
/// The registry name must be accepted as an rclone backend type; a typo
/// there would produce `config/create` failures only at runtime.
fn all_backend_names_exist_upstream() {
    let providers = mbr::rclone::call("config/providers", serde_json::json!({})).unwrap();
    let names: Vec<String> = providers["providers"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| {
            p.get("Prefix")
                .and_then(Value::as_str)
                .or_else(|| p.get("Name").and_then(Value::as_str))
                .map(str::to_owned)
        })
        .collect();
    for backend in BACKENDS {
        assert!(
            names.iter().any(|n| n == backend.name),
            "backend '{}' does not exist in this rclone",
            backend.name
        );
    }
}
