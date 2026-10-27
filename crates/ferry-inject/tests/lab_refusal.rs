//! The `drill` lab example creates virtual devices and presses keys, so
//! outside a lab VM it must exit 2 with `refusing` before it opens any device.
#![expect(
    clippy::print_stderr,
    reason = "integration test: diagnostics for a failing run, which the harness captures"
)]

use std::process::Command;

/// True on a machine where the example is allowed to run, where this test has nothing
/// to prove and must not start a drill.
fn is_lab() -> bool {
    std::env::var_os("FERRY_LAB").is_some_and(|v| v == "1")
        || ["product_name", "sys_vendor"].iter().any(|f| {
            std::fs::read_to_string(format!("/sys/class/dmi/id/{f}"))
                .is_ok_and(|s| s.contains("QEMU") || s.contains("KVM") || s.contains("Standard PC"))
        })
}

#[test]
fn drill_refuses_outside_a_lab_before_opening_a_device() {
    if is_lab() {
        eprintln!("skipped: this is a lab machine, where the drill would run");
        return;
    }
    let exe = std::env::current_exe().expect("test exe path");
    let drill = exe
        .parent()
        .and_then(|deps| deps.parent())
        .expect("target profile dir")
        .join("examples")
        .join(format!("drill{}", std::env::consts::EXE_SUFFIX));
    assert!(drill.exists(), "example not built: {}", drill.display());
    for sub in ["hold", "aim", "keymap", "flood"] {
        let out = Command::new(&drill)
            .arg(sub)
            .env_remove("FERRY_LAB")
            .output()
            .expect("run drill");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "drill {sub}: {stderr}");
        assert!(stderr.contains("refusing"), "drill {sub}: {stderr}");
        assert!(out.stdout.is_empty(), "drill {sub} printed before refusing");
    }
}
