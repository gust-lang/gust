#[path = "integration/harness/mod.rs"]
mod harness;

macro_rules! register_integration_test {
    ($name:ident, $suite:literal, $path:literal) => {
        #[allow(non_snake_case)]
        #[test]
        fn $name() {
            crate::harness::run_discovered_fixture($suite, std::path::Path::new($path));
        }
    };
}

include!(concat!(env!("OUT_DIR"), "/integration_generated.rs"));

/// The generated `#[test]` list above is built from the checked-in
/// `tests/integration/fixtures.manifest`, not by watching the fixture
/// directories — so editing a fixture's contents rebuilds nothing
/// (metel-core#873). This test keeps discovery honest: it re-walks the source
/// tree and fails if a fixture was added / removed / renamed without
/// regenerating the manifest.
///
/// To regenerate: `UPDATE_FIXTURES=1 cargo test -p metel --test integration fixtures_manifest_is_current`
#[test]
fn fixtures_manifest_is_current() {
    let expected = harness::manifest_text();
    let path = harness::manifest_path();
    let actual = std::fs::read_to_string(&path).unwrap_or_default();

    if actual == expected {
        return;
    }

    if std::env::var_os("UPDATE_FIXTURES").is_some() {
        std::fs::write(&path, &expected)
            .unwrap_or_else(|e| panic!("failed to write {}: {e}", path.display()));
        panic!(
            "updated {} — re-run without UPDATE_FIXTURES, and commit the change",
            path.display()
        );
    }

    let on_disk: std::collections::BTreeSet<_> = expected.lines().collect();
    let recorded: std::collections::BTreeSet<_> = actual.lines().collect();
    let added: Vec<_> = on_disk.difference(&recorded).collect();
    let removed: Vec<_> = recorded.difference(&on_disk).collect();
    panic!(
        "{} is stale.\n  + on disk, not recorded: {added:#?}\n  - recorded, not on disk: {removed:#?}\n\
         run `UPDATE_FIXTURES=1 cargo test -p metel --test integration fixtures_manifest_is_current` and commit {}",
        path.display(),
        path.display()
    );
}
