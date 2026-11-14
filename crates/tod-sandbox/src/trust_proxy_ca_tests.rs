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

fn pip_conf(t: &tempfile::TempDir, existing: &str) -> String {
    with_ca(t.path());
    std::fs::write(t.path().join("fs/etc/pip.conf"), existing).unwrap();
    let o = run(t.path(), None);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    std::fs::read_to_string(t.path().join("fs/etc/pip.conf")).unwrap()
}

#[test]
fn an_existing_pip_conf_gets_the_cert_in_its_global_section() {
    let t = tempfile::tempdir().unwrap();
    let c = pip_conf(&t, "[global]\nindex-url = https://x\n[install]\nuser = true\n");
    assert!(c.contains("[global]\ncert = "), "{c}");
    assert!(c.contains("index-url = https://x") && c.contains("[install]\nuser = true"));
    // A rerun adds nothing more.
    assert!(run(t.path(), None).status.success());
    let again = std::fs::read_to_string(t.path().join("fs/etc/pip.conf")).unwrap();
    assert_eq!(again.matches("cert = ").count(), 1);
    let t = tempfile::tempdir().unwrap();
    let c = pip_conf(&t, "[install]\nuser = true\n");
    assert!(c.contains("[install]\nuser = true") && c.contains("[global]\ncert = "), "{c}");
}

#[test]
fn bootstrap_trusts_the_ca_before_any_package_manager() {
    let src = include_str!("../../../assets/sandbox/bootstrap.sh");
    let marker = src.find("#@trust-proxy-ca@").expect("marker");
    for pm in ["apt-get update", "apk add", "dnf install", "microdnf install", "yum install"] {
        let first = src.find(pm).unwrap();
        assert!(marker < first, "the CA is trusted before `{pm}`");
    }
    let script = String::from_utf8(crate::provision::bootstrap_script().to_vec()).unwrap();
    assert!(!script.contains("#@trust-proxy-ca@"));
    let trust = script.find("trust-proxy-ca: ").unwrap();
    assert!(trust < script.find("apt-get update").unwrap());
    // It parses, and it runs the trust step first: with a proxy and no CA it stops there.
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("bootstrap.sh");
    std::fs::write(&path, &script).unwrap();
    assert!(Command::new("sh").arg("-n").arg(&path).status().unwrap().success());
    let o = Command::new("sh")
        .arg(&path)
        .env("HTTPS_PROXY", "http://localhost:1")
        .env("TOD_CA_ROOT", t.path().join("fs"))
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("proxy CA"), "{}", String::from_utf8_lossy(&o.stderr));
}

#[test]
fn the_install_trusts_the_ca_before_it_installs_curl() {
    let node = include_str!("node.rs");
    let at = node.find("pub fn install(bx").unwrap();
    let body = &node[at..at + 700];
    assert!(body.find("trust_proxy_ca(").unwrap() < body.find("ensure_curl(").unwrap());
}

#[test]
fn no_proxy_is_a_no_op() {
    let t = tempfile::tempdir().unwrap();
    let o = run(t.path(), None);
    assert!(o.status.success());
    assert!(!t.path().join("fs").exists());
}

#[test]
fn a_sandbox_with_the_ca_file_but_no_proxy_variables_still_trusts_it() {
    let t = tempfile::tempdir().unwrap();
    with_ca(t.path());
    let o = run(t.path(), None);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(t.path().join("fs/etc/profile.d/tod-ca.sh").exists());
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
