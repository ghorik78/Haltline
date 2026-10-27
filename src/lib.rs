//! Zed extension: registers the Haltline debug adapter for STM32 / Cortex-M.
//!
//! The adapter itself (`haltline-dap`) is a native program. It is taken from
//! (in this order) the `dap.Haltline.binary` setting, `adapterPath` in the
//! debug configuration, `PATH`, or downloaded from this project's GitHub
//! release matching the extension version.

mod logic;

use logic::{Request, ADAPTER_NAME};
use std::fs;
use zed_extension_api::{
    self as zed, serde_json, DebugAdapterBinary, DebugConfig, DebugRequest, DebugScenario,
    DebugTaskDefinition, StartDebuggingRequestArguments, StartDebuggingRequestArgumentsRequest,
    Worktree,
};

struct HaltlineExtension {
    cached_binary: Option<String>,
}

fn to_zed(kind: Request) -> StartDebuggingRequestArgumentsRequest {
    match kind {
        Request::Launch => StartDebuggingRequestArgumentsRequest::Launch,
        Request::Attach => StartDebuggingRequestArgumentsRequest::Attach,
    }
}

/// Absolute path of the extension's work directory.
fn work_dir() -> String {
    std::env::var("PWD")
        .ok()
        .filter(|p| !p.is_empty())
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
        })
        .unwrap_or_default()
}

fn join(dir: &str, file: &str) -> String {
    if dir.is_empty() {
        return file.to_string();
    }
    let sep = if dir.contains('\\') && !dir.contains('/') {
        '\\'
    } else {
        '/'
    };
    format!("{}{sep}{file}", dir.trim_end_matches(['/', '\\']))
}

impl HaltlineExtension {
    fn adapter_binary(
        &mut self,
        user_path: Option<String>,
        config: &serde_json::Value,
        worktree: &Worktree,
    ) -> Result<String, String> {
        if let Some(path) = user_path.filter(|p| !p.trim().is_empty()) {
            return Ok(path);
        }
        if let Some(path) = config
            .get("adapterPath")
            .and_then(serde_json::Value::as_str)
            .filter(|p| !p.trim().is_empty())
        {
            return Ok(path.to_string());
        }
        if let Some(path) = worktree.which(logic::BINARY_NAME) {
            return Ok(path);
        }
        if let Some(path) = &self.cached_binary {
            if fs::metadata(path).is_ok_and(|m| m.is_file()) {
                return Ok(path.clone());
            }
        }
        let path = self.download_adapter()?;
        self.cached_binary = Some(path.clone());
        Ok(path)
    }

    /// Downloads the adapter release matching this extension version
    /// (or the latest release if that one has no binary for this platform).
    fn download_adapter(&self) -> Result<String, String> {
        let (os, arch) = zed::current_platform();
        let os_name = match os {
            zed::Os::Mac => "macos",
            zed::Os::Linux => "linux",
            zed::Os::Windows => "windows",
        };
        let arch_name = match arch {
            zed::Architecture::Aarch64 => "aarch64",
            zed::Architecture::X8664 => "x86_64",
            zed::Architecture::X86 => "x86",
        };
        let triple = logic::target_triple(os_name, arch_name).ok_or_else(|| {
            format!(
                "no prebuilt haltline-dap for {os_name}/{arch_name}; build it with \
                 `cargo install --path adapter` and put it on PATH"
            )
        })?;
        let binary = logic::binary_file_name(logic::is_windows_triple(triple));

        let tag = logic::release_tag();
        let dir = logic::install_dir(&tag);
        let local = join(&dir, &binary);
        if fs::metadata(&local).is_ok_and(|m| m.is_file()) {
            return Ok(join(&work_dir(), &local));
        }

        let asset_name = logic::asset_name(triple);
        let release = zed::github_release_by_tag_name(logic::GITHUB_REPO, &tag)
            .ok()
            .filter(|r| r.assets.iter().any(|a| a.name == asset_name))
            .map(Ok)
            .unwrap_or_else(|| {
                zed::latest_github_release(
                    logic::GITHUB_REPO,
                    zed::GithubReleaseOptions {
                        require_assets: true,
                        pre_release: false,
                    },
                )
            })
            .map_err(|e| format!("cannot find a haltline-dap release on GitHub: {e}"))?;
        let asset = release
            .assets
            .iter()
            .find(|a| a.name == asset_name)
            .ok_or_else(|| format!("release {} has no asset {asset_name}", release.version))?;
        let dir = logic::install_dir(&release.version);
        let local = join(&dir, &binary);
        if !fs::metadata(&local).is_ok_and(|m| m.is_file()) {
            let file_type = if asset_name.ends_with(".zip") {
                zed::DownloadedFileType::Zip
            } else {
                zed::DownloadedFileType::GzipTar
            };
            zed::download_file(&asset.download_url, &dir, file_type)
                .map_err(|e| format!("failed to download {asset_name}: {e}"))?;
            zed::make_file_executable(&local)?;
            if let Ok(entries) = fs::read_dir(".") {
                let names: Vec<String> = entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect();
                for stale in logic::stale_install_dirs(names.iter().map(String::as_str), &dir) {
                    let _ = fs::remove_dir_all(stale);
                }
            }
        }
        Ok(join(&work_dir(), &local))
    }
}

impl zed::Extension for HaltlineExtension {
    fn new() -> Self {
        Self {
            cached_binary: None,
        }
    }

    fn get_dap_binary(
        &mut self,
        adapter_name: String,
        config: DebugTaskDefinition,
        user_provided_debug_adapter_path: Option<String>,
        worktree: &Worktree,
    ) -> Result<DebugAdapterBinary, String> {
        if adapter_name != ADAPTER_NAME {
            return Err(format!("unknown debug adapter `{adapter_name}`"));
        }
        let mut configuration: serde_json::Value = serde_json::from_str(&config.config)
            .map_err(|e| format!("invalid debug configuration: {e}"))?;
        let request = logic::request_kind(&configuration)?;
        let command =
            self.adapter_binary(user_provided_debug_adapter_path, &configuration, worktree)?;
        let root = worktree.root_path();
        logic::fill_defaults(&mut configuration, &root);
        Ok(DebugAdapterBinary {
            command: Some(command),
            arguments: Vec::new(),
            envs: worktree.shell_env(),
            cwd: Some(root),
            connection: None,
            request_args: StartDebuggingRequestArguments {
                configuration: configuration.to_string(),
                request: to_zed(request),
            },
        })
    }

    fn dap_request_kind(
        &mut self,
        _adapter_name: String,
        config: serde_json::Value,
    ) -> Result<StartDebuggingRequestArgumentsRequest, String> {
        logic::request_kind(&config).map(to_zed)
    }

    fn dap_config_to_scenario(&mut self, config: DebugConfig) -> Result<DebugScenario, String> {
        let adapter_config = match config.request {
            DebugRequest::Launch(launch) => logic::launch_scenario_config(
                &launch.program,
                launch.cwd.as_deref(),
                config.stop_on_entry,
            ),
            DebugRequest::Attach(_) => logic::attach_scenario_config(),
        };
        Ok(DebugScenario {
            label: config.label,
            adapter: config.adapter,
            build: None,
            config: adapter_config.to_string(),
            tcp_connection: None,
        })
    }
}

zed::register_extension!(HaltlineExtension);
