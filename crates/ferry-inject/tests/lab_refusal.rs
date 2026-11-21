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

/// The built example `name`. Cargo puts examples in `<target>/<profile>/examples/`, but the test
/// binary's own place depends on the cargo version: `<profile>/deps/` up to 1.99, and
/// `<profile>/build/<pkg>/<hash>/out/` from 1.100. So look in every directory above it.
fn example(name: &str) -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("test exe path");
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    exe.ancestors()
        .skip(1)
        .map(|dir| dir.join("examples").join(&file))
        .find(|p| p.is_file())
        .unwrap_or_else(|| {
            panic!(
                "example {name} not built: no examples/{file} above {}",
                exe.display()
            )
        })
}

#[test]
fn drill_refuses_outside_a_lab_before_opening_a_device() {
    if is_lab() {
        eprintln!("skipped: this is a lab machine, where the drill would run");
        return;
    }
    let drill = example("drill");
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
