//! Where a bare credential name resolves, and how a credential an earlier
//! release left beside `arsy.json` reaches the `secrets` directory.
//!
//! One test in its own binary: it points `ARSY_CONFIG_HOME` at a temporary
//! directory, and no other test may observe that while it runs.

use arsy_kernel::config::{home_file, CACHE_DIRECTORY, CONFIG_HOME_VAR, SECRETS_DIRECTORY};
use arsy_kernel::secret::{CredentialStore, FileCredentialStore};
use std::path::Path;

fn owner_only(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn bare_credentials_live_in_the_secrets_directory_and_old_ones_are_moved() {
    let home = tempfile::tempdir().unwrap();
    std::env::set_var(CONFIG_HOME_VAR, home.path());
    let secrets = home.path().join(SECRETS_DIRECTORY);

    // A bare name resolves under `secrets/`, which is made owner-only.
    let path = FileCredentialStore::path("fresh.key").unwrap();
    assert_eq!(path, secrets.join("fresh.key"));
    #[cfg(unix)]
    assert_eq!(mode(&secrets), 0o700, "the secrets directory is owner-only");
    FileCredentialStore
        .set("fresh.key", "sk-fresh-0123456789")
        .unwrap();
    assert!(secrets.join("fresh.key").is_file());
    assert!(!home.path().join("fresh.key").exists());

    // A credential an earlier release wrote beside arsy.json moves on its
    // first resolve, keeping its content and its owner-only mode.
    let legacy = home.path().join("legacy.key");
    std::fs::write(&legacy, "sk-legacy-0123456789\n").unwrap();
    owner_only(&legacy);
    assert_eq!(
        FileCredentialStore.resolve("legacy.key").unwrap(),
        "sk-legacy-0123456789"
    );
    assert!(!legacy.exists(), "the old file moved");
    assert!(secrets.join("legacy.key").is_file());
    #[cfg(unix)]
    assert_eq!(mode(&secrets.join("legacy.key")), 0o600);

    // Never over a file already in `secrets/`: that one is in use.
    let stale = home.path().join("both.key");
    std::fs::write(&stale, "sk-stale-0123456789").unwrap();
    owner_only(&stale);
    std::fs::write(secrets.join("both.key"), "sk-current-0123456789").unwrap();
    owner_only(&secrets.join("both.key"));
    assert_eq!(
        FileCredentialStore.resolve("both.key").unwrap(),
        "sk-current-0123456789"
    );
    assert_eq!(
        std::fs::read_to_string(&stale).unwrap(),
        "sk-stale-0123456789",
        "the old file is left alone"
    );

    // An absolute handle is the operator's own path and is never moved.
    let elsewhere = tempfile::tempdir().unwrap();
    let absolute = elsewhere.path().join("mine.key");
    std::fs::write(&absolute, "sk-absolute-0123456789").unwrap();
    owner_only(&absolute);
    let name = absolute.display().to_string();
    assert_eq!(
        FileCredentialStore.resolve(&name).unwrap(),
        "sk-absolute-0123456789"
    );
    assert!(absolute.is_file());

    // A name that walks out of the directory moves nothing and is refused.
    std::fs::write(home.path().join("outside"), "x").unwrap();
    assert!(FileCredentialStore.resolve("../outside").is_err());
    assert!(home.path().join("outside").is_file());

    // The same carry-over serves caches.
    std::fs::write(home.path().join("mcp-tools.json"), "{}").unwrap();
    let cache = home_file(CACHE_DIRECTORY, "mcp-tools.json").unwrap();
    assert_eq!(
        cache,
        home.path().join(CACHE_DIRECTORY).join("mcp-tools.json")
    );
    assert_eq!(std::fs::read_to_string(&cache).unwrap(), "{}");
    assert!(!home.path().join("mcp-tools.json").exists());

    std::env::remove_var(CONFIG_HOME_VAR);
}
