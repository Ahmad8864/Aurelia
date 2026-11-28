use super::super::*;
use crate::core::error::classify;
use std::cell::Cell;

#[derive(Clone, Copy, Default)]
struct FakeSteam {
    running: bool,
    stops: usize,
    starts: usize,
    start_fails: bool,
}

thread_local! {
    // Per test thread, so parallel tests drive separate fakes.
    static STEAM: Cell<FakeSteam> = Cell::default();
}

fn steam() -> FakeSteam {
    STEAM.get()
}

fn set_steam(edit: impl FnOnce(&mut FakeSteam)) {
    let mut fake = STEAM.get();
    edit(&mut fake);
    STEAM.set(fake);
}

fn running() -> bool {
    steam().running
}

fn stop() -> Result<()> {
    set_steam(|s| {
        s.stops += 1;
        s.running = false;
    });
    Ok(())
}

fn start() -> Result<()> {
    set_steam(|s| s.starts += 1);
    if steam().start_fails {
        bail!("Steam failed to start");
    }
    set_steam(|s| s.running = true);
    Ok(())
}

static FAKE: SteamControl = SteamControl {
    running,
    stop,
    start,
};

fn hold() -> &'static Hold {
    Box::leak(Box::new(Hold::new(&FAKE)))
}

fn existing_file(dir: &Path) -> PathBuf {
    let path = dir.join("appmanifest_1.acf");
    std::fs::write(&path, "\"AppState\"\n{\n}\n").unwrap();
    path
}

#[test]
fn held_files_refuse_and_fresh_files_proceed() {
    let dir = tempfile::tempdir().unwrap();
    let held = existing_file(dir.path());
    let fresh = dir.path().join("appmanifest_2.acf");
    let hold = hold();
    let refuse = SteamClient::new().unwrap();
    set_steam(|s| s.running = true);

    let files = vec![fresh.clone(), held.clone()];
    let Err(err) = hold.lease(files, refuse.steam_files.clone()) else {
        panic!("a held file was leased while Steam runs");
    };
    assert_eq!(classify(&err).kind, ErrorKind::SteamRunning);
    drop(hold.lease(vec![fresh], refuse.steam_files.clone()).unwrap());
    assert_eq!(steam().stops, 0, "a fresh file proceeds with Steam open");

    let restart = SteamClient::new().unwrap().allow_steam_restart(true);
    drop(
        hold.lease(vec![held.clone()], restart.steam_files.clone())
            .unwrap(),
    );
    assert_eq!((steam().stops, steam().starts), (1, 1));
    assert!(restart.steam_files_report().unwrap());
    assert!(!refuse.steam_files_report().unwrap());

    set_steam(|s| s.running = false);
    drop(hold.lease(vec![held], refuse.steam_files.clone()).unwrap());
    assert_eq!(steam().stops, 1, "a closed Steam holds nothing");
}

#[test]
fn steam_restarts_once_after_overlap_even_when_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let held = existing_file(dir.path());
    let hold = hold();
    let restart = SteamClient::new().unwrap().allow_steam_restart(true);
    set_steam(|s| s.running = true);

    let cancelled = hold
        .lease(vec![held.clone()], restart.steam_files.clone())
        .unwrap();
    let overlapping = hold
        .lease(vec![held.clone()], SteamFileAccess::default())
        .unwrap();
    overlapping.finish();
    assert_eq!(steam().starts, 0, "Steam stays down while a change runs");
    drop(cancelled);
    assert_eq!((steam().stops, steam().starts), (1, 1));
    assert!(restart.steam_files_report().unwrap());

    set_steam(|s| s.start_fails = true);
    let failing = SteamClient::new().unwrap().allow_steam_restart(true);
    hold.lease(vec![held], failing.steam_files.clone())
        .unwrap()
        .finish();
    let err = failing.steam_files_report().unwrap_err();
    assert_eq!(err.to_string(), "Steam failed to start");
}
