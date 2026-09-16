use std::{fs, process::Command};

#[test]
fn cli_indexes_resumes_and_reports_uac_sources() {
    let dir = tempfile::tempdir().unwrap();
    let uac = dir.path().join("collection");
    fs::create_dir_all(uac.join("bodyfile")).unwrap();
    fs::write(
        uac.join("bodyfile/bodyfile.txt"),
        "0|/sample|1|mode|0|0|2|1|2|3|4\ninvalid\n",
    )
    .unwrap();
    fs::write(uac.join("uac.log"), "INFO Hostname: test-host\n").unwrap();
    let rules = dir.path().join("rules.yaml");
    fs::write(&rules, "[]").unwrap();
    let workspace = dir.path().join("workspace");
    let exe = env!("CARGO_BIN_EXE_timeliner-tui");
    let indexed = Command::new(exe)
        .arg("--index-only")
        .arg("--workspace")
        .arg(&workspace)
        .arg("--ioc-file")
        .arg(&rules)
        .args(["--theme", "dark"])
        .arg(&uac)
        .output()
        .unwrap();
    assert!(
        indexed.status.success(),
        "{}",
        String::from_utf8_lossy(&indexed.stderr)
    );
    let result = Command::new(exe)
        .arg("--workspace")
        .arg(&workspace)
        .arg("--diagnostics")
        .output()
        .unwrap();
    assert!(result.status.success());
    let text = String::from_utf8_lossy(&result.stdout);
    assert!(text.contains("test-host"));
    assert!(text.contains("Malformed records: 1"));
    let resumed = Command::new(exe)
        .arg("--workspace")
        .arg(&workspace)
        .arg("--index-only")
        .output()
        .unwrap();
    assert!(resumed.status.success());
    let store = timeliner_tui::store::Store::reader(&workspace).unwrap();
    assert_eq!(
        store.session().unwrap().theme,
        timeliner_tui::theme::Theme::Dark
    );
}

#[test]
fn cli_rejects_a_nonexistent_resume_workspace_and_unknown_timezone() {
    let dir = tempfile::tempdir().unwrap();
    let exe = env!("CARGO_BIN_EXE_timeliner-tui");
    let missing = Command::new(exe)
        .arg("--workspace")
        .arg(dir.path().join("missing"))
        .arg("--index-only")
        .output()
        .unwrap();
    assert!(!missing.status.success());
    let invalid = Command::new(exe)
        .args(["--tz", "Not/AZone"])
        .output()
        .unwrap();
    assert!(!invalid.status.success());
}

#[test]
fn resuming_a_workspace_restarts_its_incomplete_imports() {
    use timeliner_tui::{ioc::Rules, source, store::Store};
    let dir = tempfile::tempdir().unwrap();
    let body = dir.path().join("source.body");
    fs::write(&body, "0|/sample|1|mode|0|0|2|1|2|3|4\n").unwrap();
    let workspace = dir.path().join("workspace");
    let mut store = Store::open(&workspace).unwrap();
    assert!(
        store
            .import(
                &source::resolve(&body, None, false).unwrap(),
                &Rules::default(),
                |_, _| false
            )
            .is_err()
    );
    assert_eq!(store.pending_inputs().unwrap().len(), 1);
    drop(store);
    let result = Command::new(env!("CARGO_BIN_EXE_timeliner-tui"))
        .arg("--workspace")
        .arg(&workspace)
        .arg("--index-only")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let store = Store::reader(&workspace).unwrap();
    assert!(store.pending_inputs().unwrap().is_empty());
    assert_eq!(store.sources().unwrap()[0].sha256.len(), 32);
}
