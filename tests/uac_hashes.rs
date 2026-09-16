use std::{fs, path::Path};
use tempfile::TempDir;
use timeliner_tui::{
    ioc::Rules,
    query::Cancellation,
    source,
    store::{SourceInfo, Store},
    uac,
};

const MD5: &str = "0123456789abcdef0123456789abcdef";
const OTHER_MD5: &str = "fedcba9876543210fedcba9876543210";
const SHA1: &str = "0123456789abcdef0123456789abcdef01234567";

fn package(store: &mut Store, root: &Path, name: &str) -> SourceInfo {
    let root = root.join(name);
    fs::create_dir_all(root.join("bodyfile")).unwrap();
    fs::create_dir_all(root.join("hash_executables")).unwrap();
    fs::write(
        root.join("bodyfile/bodyfile.txt"),
        "0|/usr/bin/tool|1|-rwxr-xr-x|0|0|10|100|200|300|400\n",
    )
    .unwrap();
    store
        .import(
            &source::resolve(&root, Some("same-host"), false).unwrap(),
            &Rules::default(),
            |_, _| true,
        )
        .unwrap();
    store
        .sources()
        .unwrap()
        .into_iter()
        .find(|source| source.path.starts_with(root.as_path()))
        .unwrap()
}

fn setup() -> (TempDir, Store, SourceInfo) {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("workspace")).unwrap();
    let source = package(&mut store, dir.path(), "uac");
    (dir, store, source)
}

fn manifest(source: &SourceInfo, name: &str, contents: &str) -> std::path::PathBuf {
    let path = source
        .path
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("hash_executables")
        .join(name);
    fs::write(&path, contents).unwrap();
    path
}

#[test]
fn native_checksum_formats_preserve_exact_paths_and_provenance() {
    let (_dir, store, source) = setup();
    let md5_file = manifest(
        &source,
        "hash_executables.md5",
        &format!(
            "ignored diagnostic\n{MD5}  /usr/bin/tool\n{MD5} */usr/bin/binary\n\
         {MD5}  /usr/bin/with spaces \n{MD5}  /usr/bin/a=b\n\
         \\{MD5}  /usr/bin/back\\\\slash\n{MD5} /usr/bin/reverse\n"
        ),
    );
    let sha1_file = manifest(
        &source,
        "hash_executables.sha1",
        &format!("SHA1 (/usr/bin/tool) = {SHA1}\nSHA1(/usr/bin/parentheses (x)=y)= {SHA1}\n"),
    );
    for path in [
        "/usr/bin/tool",
        "/usr/bin/binary",
        "/usr/bin/with spaces ",
        "/usr/bin/a=b",
        r"/usr/bin/back\slash",
        "/usr/bin/reverse",
    ] {
        let hashes = uac::executable_hashes(&store, &source, path, &Cancellation::never())
            .unwrap()
            .unwrap();
        assert!(hashes.warnings.is_empty(), "{:?}", hashes.warnings);
        assert_eq!(hashes.values[0].digest, MD5);
        assert_eq!(hashes.values[0].manifest, md5_file);
        if path == "/usr/bin/tool" {
            assert_eq!(hashes.values[0].line, 2);
            assert_eq!(hashes.values[1].digest, SHA1);
            assert_eq!(hashes.values[1].algorithm, "SHA-1");
            assert_eq!(hashes.values[1].manifest, sha1_file);
        }
    }
    let hashes = uac::executable_hashes(
        &store,
        &source,
        "/usr/bin/parentheses (x)=y",
        &Cancellation::never(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(hashes.values[0].digest, SHA1);
    for path in [
        "/other/tool",
        "/usr/bin/with spaces",
        "/USR/bin/tool",
        "/usr/bin/tool.old",
    ] {
        assert!(
            uac::executable_hashes(&store, &source, path, &Cancellation::never())
                .unwrap()
                .unwrap()
                .values
                .is_empty()
        );
    }
}

#[test]
fn multiple_manifests_keep_conflicts_visible_and_deduplicate_identical_hashes() {
    let (_dir, store, source) = setup();
    manifest(
        &source,
        "hash_first.md5",
        &format!("MD5 (/usr/bin/tool) = {}\n", MD5.to_ascii_uppercase()),
    );
    manifest(
        &source,
        "hash_second.md5",
        &format!("{MD5}  /usr/bin/tool\n{OTHER_MD5}  /usr/bin/tool\n"),
    );
    manifest(
        &source,
        "not-a-hash.md5",
        &format!("{OTHER_MD5}  /usr/bin/not-collected\n"),
    );
    let hashes = uac::executable_hashes(&store, &source, "/usr/bin/tool", &Cancellation::never())
        .unwrap()
        .unwrap();
    assert_eq!(hashes.values.len(), 2);
    assert_eq!(hashes.values[0].digest, MD5);
    assert_eq!(hashes.values[1].digest, OTHER_MD5);
}

#[test]
fn matching_paths_and_hostnames_from_other_packages_never_supply_hashes() {
    let (dir, mut store, first) = setup();
    let second = package(&mut store, dir.path(), "other-uac");
    manifest(
        &first,
        "hash_executables.md5",
        &format!("{MD5}  /usr/bin/tool\n"),
    );
    manifest(
        &second,
        "hash_executables.md5",
        &format!("{OTHER_MD5}  /usr/bin/tool\n"),
    );
    for (source, expected) in [(&first, MD5), (&second, OTHER_MD5)] {
        let hashes =
            uac::executable_hashes(&store, source, "/usr/bin/tool", &Cancellation::never())
                .unwrap()
                .unwrap();
        assert_eq!(hashes.values.len(), 1);
        assert_eq!(hashes.values[0].digest, expected);
    }
}

#[test]
fn hash_cache_survives_reopen_and_refreshes_changed_or_removed_manifests() {
    let (_dir, store, source) = setup();
    let file = manifest(
        &source,
        "hash_executables.md5",
        &format!("{MD5}  /usr/bin/tool\n"),
    );
    uac::executable_hashes(&store, &source, "/usr/bin/tool", &Cancellation::never()).unwrap();
    let revision = store.revision().unwrap();
    let root = store.root.clone();
    drop(store);
    let store = Store::open(&root).unwrap();
    let cached = uac::executable_hashes(&store, &source, "/usr/bin/tool", &Cancellation::never())
        .unwrap()
        .unwrap();
    assert_eq!(cached.values[0].digest, MD5);
    fs::write(&file, format!("{OTHER_MD5}  /usr/bin/tool\n\n")).unwrap();
    let changed = uac::executable_hashes(&store, &source, "/usr/bin/tool", &Cancellation::never())
        .unwrap()
        .unwrap();
    assert_eq!(changed.values[0].digest, OTHER_MD5);
    assert_eq!(store.revision().unwrap(), revision);
    fs::remove_file(file).unwrap();
    assert!(
        uac::executable_hashes(&store, &source, "/usr/bin/tool", &Cancellation::never())
            .unwrap()
            .is_none()
    );
}

#[test]
fn cached_hash_lines_are_verified_even_if_file_metadata_is_unchanged() {
    let (_dir, store, source) = setup();
    let file = manifest(
        &source,
        "hash_executables.md5",
        &format!("{MD5}  /usr/bin/tool\n"),
    );
    let modified = file.metadata().unwrap().modified().unwrap();
    uac::executable_hashes(&store, &source, "/usr/bin/tool", &Cancellation::never()).unwrap();
    fs::write(&file, format!("{OTHER_MD5}  /usr/bin/tool\n")).unwrap();
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    let hashes = uac::executable_hashes(&store, &source, "/usr/bin/tool", &Cancellation::never())
        .unwrap()
        .unwrap();
    assert!(hashes.values.is_empty());
    assert!(hashes.warnings[0].contains("changed"));
}

#[test]
fn malformed_missing_and_oversized_hash_lines_do_not_break_details() {
    let (_dir, store, source) = setup();
    assert!(
        uac::executable_hashes(&store, &source, "/usr/bin/tool", &Cancellation::never())
            .unwrap()
            .is_none()
    );
    let mut contents = "x".repeat(1_048_600);
    contents.push_str(&format!(
        "\nnot-a-digest  /usr/bin/tool\n{SHA1}  /usr/bin/tool\n{MD5}  /usr/bin/tool\n"
    ));
    manifest(&source, "hash_executables.md5", &contents);
    let hashes = uac::executable_hashes(&store, &source, "/usr/bin/tool", &Cancellation::never())
        .unwrap()
        .unwrap();
    assert_eq!(hashes.values.len(), 1);
    assert_eq!(hashes.values[0].line, 4);
    assert_eq!(hashes.values[0].digest, MD5);
    let cancel = Cancellation::never();
    cancel
        .clock
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    assert!(uac::executable_hashes(&store, &source, "/usr/bin/tool", &cancel).is_err());
}
