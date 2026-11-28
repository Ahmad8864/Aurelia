use std::path::Path;

// Files under src/steam_client that may write without a lease.
const ALLOWED: &[&str] = &[
    // The lease itself.
    "steam_files.rs",
    // steam.cfg, read by Steam's bootstrapper, is not a library record.
    "process.rs",
    // Workshop records stay out until a running Steam is shown to hold them.
    "workshop_manifest.rs",
];

fn unleased_writes(dir: &Path, found: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            if name != "tests" {
                unleased_writes(&path, found);
            }
            continue;
        }
        if !name.ends_with(".rs") || name == "tests.rs" || ALLOWED.contains(&name.as_str()) {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for call in ["write_atomic(", "fs::write(", "fs::copy(", "fs::rename("] {
            if text.contains(call) {
                found.push(format!("{} calls {call}", path.display()));
            }
        }
    }
}

#[test]
fn steam_files_change_only_through_a_lease() {
    let mut found = Vec::new();
    unleased_writes(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src/steam_client"),
        &mut found,
    );
    assert!(
        found.is_empty(),
        "write through a steam_files::Lease: {found:#?}"
    );
}
