//! The proxy-CA script (`assets/sandbox/trust-proxy-ca.sh`), run for real
//! against a temporary root.

use crate::node::TRUST_PROXY_CA_SCRIPT;
use std::process::Command;

fn run(root: &std::path::Path, proxy: Option<&str>) -> std::process::Output {
    let script = root.join("script.sh");
    std::fs::write(&script, TRUST_PROXY_CA_SCRIPT).unwrap();
    let mut c = Command::new("sh");
    c.arg(&script)
        .env_remove("HTTPS_PROXY")
        .env_remove("https_proxy")
        .env("TOD_CA_ROOT", root.join("fs"))
        .env("TOD_CA_SKIP_UPDATE", "1");
    if let Some(p) = proxy {
        c.env("HTTPS_PROXY", p);
    }
    c.output().unwrap()
}

fn with_ca(root: &std::path::Path) {
    let d = root.join("fs/etc/ssl/certs/proxy");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("proxy-ca.pem"), "CA-BYTES").unwrap();
}

#[test]
fn no_proxy_is_a_no_op() {
    let t = tempfile::tempdir().unwrap();
    let o = run(t.path(), None);
    assert!(o.status.success());
    assert!(!t.path().join("fs").exists());
}

#[test]
fn a_proxy_without_its_ca_is_an_explicit_error() {
    let t = tempfile::tempdir().unwrap();
    let o = run(t.path(), Some("http://localhost:49152"));
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("missing or empty"));
}

#[test]
fn the_ca_is_installed_python_is_pointed_at_the_bundle_and_a_rerun_is_idempotent() {
    let t = tempfile::tempdir().unwrap();
    with_ca(t.path());
    for _ in 0..2 {
        let o = run(t.path(), Some("http://localhost:49152"));
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
    let fs = t.path().join("fs");
    let found = ["usr/local/share/ca-certificates/tod-proxy-ca.crt", "etc/pki/ca-trust/source/anchors/tod-proxy-ca.crt"]
        .iter()
        .any(|d| std::fs::read_to_string(fs.join(d)).is_ok_and(|s| s == "CA-BYTES"));
    assert!(found);
    let profile = std::fs::read_to_string(fs.join("etc/profile.d/tod-ca.sh")).unwrap();
    assert!(profile.contains("REQUESTS_CA_BUNDLE=") && profile.contains("PIP_CERT="));
    assert!(std::fs::read_to_string(fs.join("etc/pip.conf")).unwrap().contains("cert = "));
}
