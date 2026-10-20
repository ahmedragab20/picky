mod common;

use std::process::Command;

use common::PICKY;

#[test]
fn prints_the_package_version() {
    for flag in ["--version", "-v"] {
        let out = Command::new(PICKY).arg(flag).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{flag}");
        assert_eq!(String::from_utf8(out.stdout).unwrap(), format!("{}\n", env!("CARGO_PKG_VERSION")), "{flag}");
    }
}
