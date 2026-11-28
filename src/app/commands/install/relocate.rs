//! Move, relink and import installs.

use super::*;

pub(crate) async fn cmd_move(
    app_id: u32,
    library: PathBuf,
    restart_steam: bool,
    json: bool,
) -> Result<()> {
    let client = authed_client().await?.allow_steam_restart(restart_steam);
    let rx = client
        .move_install(app_id, library.clone())
        .await
        .with_context(|| format!("failed to start moving app {app_id}"))?;
    drive_progress(rx, json)
        .await
        .with_context(|| format!("failed to move app {app_id}"))?;
    let steam_restarted = client.steam_files_report()?;

    if json {
        print_json(&serde_json::json!({
            "app_id": app_id,
            "status": "moved",
            "library": library.to_string_lossy(),
            "steam_restarted": steam_restarted,
        }));
    } else {
        cli_println!("Moved app {app_id} to {}.", library.display());
    }
    Ok(())
}

/// Status and verb a relocate reports.
pub(super) struct RelocateLabels {
    /// JSON `status`, e.g. `relinked`.
    pub status: &'static str,
    /// Error-message verb, e.g. `relink`.
    pub verb: &'static str,
}

/// Relink/import shared body.
pub(super) async fn relocate_and_report<F, Fut>(
    app_id: u32,
    library: PathBuf,
    restart_steam: bool,
    json: bool,
    labels: RelocateLabels,
    human: impl FnOnce(&std::path::Path) -> String,
    op: F,
) -> Result<()>
where
    F: FnOnce(SteamClient, PathBuf) -> Fut,
    Fut: Future<Output = Result<std::path::PathBuf>>,
{
    let RelocateLabels { status, verb } = labels;
    let client = authed_client().await?.allow_steam_restart(restart_steam);
    let path = op(client.clone(), library.clone())
        .await
        .with_context(|| format!("failed to {verb} app {app_id}"))?;
    let steam_restarted = client.steam_files_report()?;

    if json {
        print_json(&serde_json::json!({
            "app_id": app_id,
            "status": status,
            "library": library.to_string_lossy(),
            "install_path": path.to_string_lossy(),
            "steam_restarted": steam_restarted,
        }));
    } else {
        cli_println!("{}", human(&path));
    }
    Ok(())
}

pub(crate) async fn cmd_relink(
    app_id: u32,
    library: PathBuf,
    restart_steam: bool,
    json: bool,
) -> Result<()> {
    let human = |_: &std::path::Path| format!("Relinked app {app_id} to {}.", library.display());
    relocate_and_report(
        app_id,
        library.clone(),
        restart_steam,
        json,
        RelocateLabels {
            status: "relinked",
            verb: "relink",
        },
        human,
        |client, lib| async move { client.relink_install(app_id, lib).await },
    )
    .await
}

pub(crate) async fn cmd_import(
    app_id: u32,
    library: PathBuf,
    platform: Option<PlatformArg>,
    restart_steam: bool,
    json: bool,
) -> Result<()> {
    // Default to the OS we're running on for the depot/platform match.
    let platform: DepotPlatform = platform.map_or_else(DepotPlatform::host, Into::into);
    let human = |path: &std::path::Path| format!("Imported app {app_id} from {}.", path.display());
    relocate_and_report(
        app_id,
        library,
        restart_steam,
        json,
        RelocateLabels {
            status: "imported",
            verb: "import",
        },
        human,
        move |client, lib| async move { client.import_install(app_id, lib, platform).await },
    )
    .await
}
