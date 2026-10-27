//! Both unit variants pass `systemd-analyze verify`. The packaged unit
//! (`dist/systemd/ferryd.service`, ExecStart=/usr/bin/ferryd) and the one `ferry setup` writes
//! for a build without a package. `verify` checks that ExecStart is an executable, so each file
//! is verified with the test binary in its place. `verify` exits 0 on an unknown key, so its
//! warnings count as failures here. Where systemd-analyze is absent the test fails, so that a
//! passing run always means the units were checked. A container or CI image without systemd
//! sets `FERRY_ALLOW_NO_SYSTEMD=1` to skip it, and the skip is printed.
#![expect(
    clippy::print_stderr,
    clippy::unwrap_used,
    reason = "integration test: diagnostics for a failing run, which the harness captures; an unwrap that fails is the test failing"
)]

use std::path::Path;
use std::process::Command;

use ferry_cli::setup::unit;

fn verify(dir: &Path, name: &str, text: &str) {
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    let out = match Command::new("systemd-analyze")
        .args(["--user", "verify"])
        .arg(&path)
        .output()
    {
        Ok(o) => o,
        Err(e) if std::env::var_os("FERRY_ALLOW_NO_SYSTEMD").is_some_and(|v| v == "1") => {
            eprintln!("SKIPPED (FERRY_ALLOW_NO_SYSTEMD=1): systemd-analyze is not available: {e}");
            return;
        }
        Err(e) => panic!(
            "systemd-analyze is not available ({e}); NFR-PKG-05 cannot be checked. \
             Set FERRY_ALLOW_NO_SYSTEMD=1 to skip this test on a host without systemd."
        ),
    };
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{name}: {err}");
    assert!(
        !err.contains("Unknown key")
            && !err.contains("Failed to parse")
            && !err.contains("Unknown section"),
        "{name}: {err}"
    );
}

#[test]
fn both_unit_variants_pass_systemd_analyze_verify() {
    let dir = tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    let exe = exe.to_string_lossy();
    // packaged
    let packaged = unit::TEMPLATE.replace("ExecStart=/usr/bin/ferryd", &format!("ExecStart={exe}"));
    assert_ne!(
        packaged,
        unit::TEMPLATE,
        "the template has the packaged ExecStart"
    );
    verify(dir.path(), "ferryd.service", &packaged);
    // what `ferry setup` writes
    let rendered = unit::render(Path::new(&*exe));
    verify(dir.path(), "ferryd-user.service", &rendered);
}
