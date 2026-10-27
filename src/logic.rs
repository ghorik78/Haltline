//! Platform-independent logic of the extension (unit-tested natively).

use serde_json::{json, Map, Value};

pub const ADAPTER_NAME: &str = "Haltline";
pub const BINARY_NAME: &str = "haltline-dap";
pub const GITHUB_REPO: &str = "ghorik78/Haltline";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Launch,
    Attach,
}

/// `request` of a debug configuration; an error when it is missing or invalid.
pub fn request_kind(config: &Value) -> Result<Request, String> {
    match config.get("request").and_then(Value::as_str) {
        Some("launch") => Ok(Request::Launch),
        Some("attach") => Ok(Request::Attach),
        Some(other) => Err(format!(
            "invalid `request` value `{other}` (expected \"launch\" or \"attach\")"
        )),
        None => Err("the debug configuration needs `request`: \"launch\" or \"attach\"".into()),
    }
}

/// Fills in values the adapter needs but Zed does not provide.
pub fn fill_defaults(config: &mut Value, worktree_root: &str) {
    if !config.is_object() {
        *config = json!({});
    }
    let obj = config.as_object_mut().unwrap();
    let missing_cwd = obj
        .get("cwd")
        .and_then(Value::as_str)
        .is_none_or(|s| s.trim().is_empty());
    if missing_cwd {
        obj.insert("cwd".into(), json!(worktree_root));
    }
    // Only used by the extension itself.
    obj.remove("adapterPath");
}

/// Rust target triple of the prebuilt adapter for a platform.
pub fn target_triple(os: &str, arch: &str) -> Option<&'static str> {
    Some(match (os, arch) {
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        _ => return None,
    })
}

pub fn is_windows_triple(triple: &str) -> bool {
    triple.contains("windows")
}

/// Release asset with the adapter for `triple`.
pub fn asset_name(triple: &str) -> String {
    let ext = if is_windows_triple(triple) {
        "zip"
    } else {
        "tar.gz"
    };
    format!("{BINARY_NAME}-{triple}.{ext}")
}

pub fn binary_file_name(windows: bool) -> String {
    if windows {
        format!("{BINARY_NAME}.exe")
    } else {
        BINARY_NAME.to_string()
    }
}

/// Directory (inside the extension's work dir) for a release tag.
pub fn install_dir(tag: &str) -> String {
    format!("{BINARY_NAME}-{tag}")
}

pub fn release_tag() -> String {
    format!("v{VERSION}")
}

/// Install directories of other versions, which can be deleted.
pub fn stale_install_dirs<'a>(
    entries: impl IntoIterator<Item = &'a str>,
    keep: &str,
) -> Vec<String> {
    let prefix = format!("{BINARY_NAME}-v");
    entries
        .into_iter()
        .filter(|e| e.starts_with(&prefix) && *e != keep)
        .map(str::to_string)
        .collect()
}

/// Debug configuration for the "new session" dialog's launch tab.
pub fn launch_scenario_config(
    program: &str,
    cwd: Option<&str>,
    stop_on_entry: Option<bool>,
) -> Value {
    let mut obj = Map::new();
    obj.insert("request".into(), json!("launch"));
    obj.insert("executable".into(), json!(program.trim()));
    obj.insert("serverType".into(), json!("openocd"));
    if let Some(cwd) = cwd.filter(|c| !c.is_empty()) {
        obj.insert("cwd".into(), json!(cwd));
    }
    if let Some(stop) = stop_on_entry {
        obj.insert("stopOnEntry".into(), json!(stop));
    }
    Value::Object(obj)
}

pub fn attach_scenario_config() -> Value {
    json!({"request": "attach", "serverType": "openocd"})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn determines_request_kind() {
        assert_eq!(
            request_kind(&json!({"request": "launch"})),
            Ok(Request::Launch)
        );
        assert_eq!(
            request_kind(&json!({"request": "attach"})),
            Ok(Request::Attach)
        );
        assert!(request_kind(&json!({}))
            .unwrap_err()
            .contains("needs `request`"));
        assert!(request_kind(&json!({"request": "run"}))
            .unwrap_err()
            .contains("invalid"));
    }

    #[test]
    fn fills_defaults() {
        let mut c = json!({"request": "launch", "adapterPath": "/x"});
        fill_defaults(&mut c, "/project");
        assert_eq!(c["cwd"], "/project");
        assert!(c.get("adapterPath").is_none());
        let mut c = json!({"cwd": "/other"});
        fill_defaults(&mut c, "/project");
        assert_eq!(c["cwd"], "/other");
        let mut c = json!(null);
        fill_defaults(&mut c, "/p");
        assert_eq!(c, json!({"cwd": "/p"}));
    }

    #[test]
    fn maps_platforms_to_assets() {
        assert_eq!(
            target_triple("linux", "x86_64"),
            Some("x86_64-unknown-linux-musl")
        );
        assert_eq!(
            target_triple("macos", "aarch64"),
            Some("aarch64-apple-darwin")
        );
        assert_eq!(
            target_triple("windows", "x86_64"),
            Some("x86_64-pc-windows-msvc")
        );
        assert_eq!(target_triple("windows", "x86"), None);
        assert_eq!(
            asset_name("x86_64-unknown-linux-musl"),
            "haltline-dap-x86_64-unknown-linux-musl.tar.gz"
        );
        assert_eq!(
            asset_name("x86_64-pc-windows-msvc"),
            "haltline-dap-x86_64-pc-windows-msvc.zip"
        );
        assert_eq!(binary_file_name(true), "haltline-dap.exe");
        assert_eq!(binary_file_name(false), "haltline-dap");
        assert_eq!(install_dir("v1.2.3"), "haltline-dap-v1.2.3");
        assert_eq!(release_tag(), format!("v{}", env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn finds_stale_install_dirs() {
        let entries = ["haltline-dap-v0.1.0", "haltline-dap-v0.2.0", "notes.txt"];
        assert_eq!(
            stale_install_dirs(entries, "haltline-dap-v0.2.0"),
            ["haltline-dap-v0.1.0"]
        );
    }

    #[test]
    fn builds_scenarios() {
        let c = launch_scenario_config(" build/fw.elf ", Some("/p"), Some(true));
        assert_eq!(c["executable"], "build/fw.elf");
        assert_eq!(c["cwd"], "/p");
        assert_eq!(c["stopOnEntry"], true);
        assert_eq!(request_kind(&c), Ok(Request::Launch));
        let c = launch_scenario_config("fw.elf", None, None);
        assert!(c.get("cwd").is_none());
        assert_eq!(request_kind(&attach_scenario_config()), Ok(Request::Attach));
    }

    /// The adapter release used by the extension must match the extension.
    #[test]
    fn versions_are_in_sync() {
        let manifest: &str = include_str!("../extension.toml");
        assert!(
            manifest.contains(&format!("version = \"{VERSION}\"")),
            "extension.toml version differs from Cargo.toml"
        );
        let adapter: &str = include_str!("../adapter/Cargo.toml");
        assert!(
            adapter.contains(&format!("version = \"{VERSION}\"")),
            "adapter/Cargo.toml version differs from Cargo.toml"
        );
    }

    /// Every property documented in the schema is understood by the adapter.
    #[test]
    fn schema_is_valid_json_with_known_properties() {
        let schema: Value =
            serde_json::from_str(include_str!("../debug_adapter_schemas/Haltline.json")).unwrap();
        let props = schema["properties"].as_object().expect("properties");
        for key in [
            "request",
            "executable",
            "serverType",
            "svdFile",
            "configFiles",
            "device",
            "runToEntryPoint",
        ] {
            assert!(props.contains_key(key), "schema lacks {key}");
        }
        let config_rs: &str = include_str!("../adapter/src/config.rs");
        for key in props.keys() {
            let snake: String = key
                .chars()
                .flat_map(|c| {
                    if c.is_ascii_uppercase() {
                        vec!['_', c.to_ascii_lowercase()]
                    } else {
                        vec![c]
                    }
                })
                .collect();
            assert!(
                config_rs.contains(&format!("pub {snake}:")) || key == "adapterPath",
                "schema property `{key}` is not a field of the adapter Config"
            );
        }
    }
}
