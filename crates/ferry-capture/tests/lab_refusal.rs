//! `capture-probe` takes input capture, so outside a lab VM it must
//! exit 2 with `refusing` before it touches the portal or any device.
#![expect(
    clippy::print_stderr,
    reason = "integration test: diagnostics for a failing run, which the harness captures"
)]

use std::process::Command;

/// True on a machine where the example is allowed to run, where this test has nothing
/// to prove and must not start a probe.
fn is_lab() -> bool {
    std::env::var_os("FERRY_LAB").is_some_and(|v| v == "1")
        || ["product_name", "sys_vendor"].iter().any(|f| {
            std::fs::read_to_string(format!("/sys/class/dmi/id/{f}"))
                .is_ok_and(|s| s.contains("QEMU") || s.contains("KVM") || s.contains("Standard PC"))
        })
}

#[test]
fn capture_probe_refuses_outside_a_lab_before_touching_the_portal() {
    if is_lab() {
        eprintln!("skipped: this is a lab machine, where the probe would run");
        return;
    }
    let exe = std::env::current_exe().expect("test exe path");
    let probe = exe
        .parent()
        .and_then(|deps| deps.parent())
        .expect("target profile dir")
        .join("examples")
        .join(format!("capture-probe{}", std::env::consts::EXE_SUFFIX));
    assert!(probe.exists(), "example not built: {}", probe.display());
    for args in [
        &["run", "--edge", "right"][..],
        &["lock"],
        &["release"],
        &["observe"],
    ] {
        let out = Command::new(&probe)
            .args(args)
            .env_remove("FERRY_LAB")
            .output()
            .expect("run capture-probe");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(2),
            "capture-probe {args:?}: {stderr}"
        );
        assert!(
            stderr.contains("refusing"),
            "capture-probe {args:?}: {stderr}"
        );
        assert!(
            out.stdout.is_empty(),
            "capture-probe {args:?} printed before refusing"
        );
    }
}
