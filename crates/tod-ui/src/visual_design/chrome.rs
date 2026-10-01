//! Chrome discovery and launch arguments (design section 5.1).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::browser::{Browser, BrowserRect, Capabilities};

/// What discovery needs from the machine; injectable for tests.
pub trait Probe: Send + Sync {
    fn exists(&self, path: &Path) -> bool;
    fn env(&self, name: &str) -> Option<String>;
    /// Windows: the default value of the App Paths `chrome.exe` key.
    fn app_paths_chrome(&self) -> Option<PathBuf>;
    /// An executable on `PATH`.
    fn which(&self, name: &str) -> Option<PathBuf>;
    fn home_dir(&self) -> Option<PathBuf>;
}

/// A machine with nothing installed.
pub struct NoProbe;
impl Probe for NoProbe {
    fn exists(&self, _: &Path) -> bool {
        false
    }
    fn env(&self, _: &str) -> Option<String> {
        None
    }
    fn app_paths_chrome(&self) -> Option<PathBuf> {
        None
    }
    fn which(&self, _: &str) -> Option<PathBuf> {
        None
    }
    fn home_dir(&self) -> Option<PathBuf> {
        None
    }
}

pub struct SystemProbe;
impl Probe for SystemProbe {
    fn exists(&self, path: &Path) -> bool {
        path.is_file()
    }
    fn env(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
    fn home_dir(&self) -> Option<PathBuf> {
        self.env("HOME")
            .or_else(|| self.env("USERPROFILE"))
            .map(PathBuf::from)
    }
    fn which(&self, name: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    }
    #[cfg(windows)]
    fn app_paths_chrome(&self) -> Option<PathBuf> {
        use std::os::windows::process::CommandExt;
        const NO_WINDOW: u32 = 0x0800_0000;
        let key = r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\chrome.exe";
        for hive in ["HKLM", "HKCU"] {
            let Ok(out) = std::process::Command::new("reg")
                .args(["query", &format!(r"{hive}\{key}"), "/ve"])
                .creation_flags(NO_WINDOW)
                .output()
            else {
                continue;
            };
            if let Some(p) = parse_reg_default(&String::from_utf8_lossy(&out.stdout)) {
                return Some(p);
            }
        }
        None
    }
    #[cfg(not(windows))]
    fn app_paths_chrome(&self) -> Option<PathBuf> {
        None
    }
}

/// Parses `reg query ... /ve` output: `    (Default)    REG_SZ    C:\...\chrome.exe`.
pub fn parse_reg_default(out: &str) -> Option<PathBuf> {
    out.lines().find_map(|l| {
        let (_, rest) = l.split_once("REG_SZ")?;
        let v = rest.trim();
        (!v.is_empty()).then(|| PathBuf::from(v))
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Os {
    Windows,
    MacOs,
    Linux,
}

impl Os {
    pub fn current() -> Os {
        if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::MacOs
        } else {
            Os::Linux
        }
    }
}

pub struct Chrome {
    override_path: Option<PathBuf>,
    probe: Box<dyn Probe>,
    os: Os,
}

impl Chrome {
    pub fn new(override_path: Option<PathBuf>) -> Self {
        Self::with_probe(override_path, Box::new(SystemProbe))
    }
    pub fn with_probe(override_path: Option<PathBuf>, probe: Box<dyn Probe>) -> Self {
        Self {
            override_path,
            probe,
            os: Os::current(),
        }
    }
    pub fn with_os(mut self, os: Os) -> Self {
        self.os = os;
        self
    }
}

impl Browser for Chrome {
    fn id(&self) -> &'static str {
        "chrome"
    }

    fn discover(&self) -> Option<PathBuf> {
        if let Some(p) = &self.override_path {
            // An override that does not exist is an error, not a reason to
            // fall back to a different Chrome.
            return self.probe.exists(p).then(|| p.clone());
        }
        let p = &*self.probe;
        match self.os {
            Os::Windows => {
                if let Some(path) = p.app_paths_chrome() {
                    if p.exists(&path) {
                        return Some(path);
                    }
                }
                ["ProgramFiles", "ProgramFiles(x86)", "LocalAppData"]
                    .iter()
                    .filter_map(|v| p.env(v))
                    .map(|d| PathBuf::from(d).join(r"Google\Chrome\Application\chrome.exe"))
                    .find(|c| p.exists(c))
            }
            Os::MacOs => {
                let rel = "Google Chrome.app/Contents/MacOS/Google Chrome";
                let mut roots = vec![PathBuf::from("/Applications")];
                if let Some(h) = p.home_dir() {
                    roots.push(h.join("Applications"));
                }
                roots.into_iter().map(|r| r.join(rel)).find(|c| p.exists(c))
            }
            Os::Linux => ["google-chrome", "google-chrome-stable"]
                .iter()
                .find_map(|n| p.which(n)),
        }
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            app_mode: true,
            launch_placement: true,
            separate_profile: true,
        }
    }

    fn debug_args(&self) -> Vec<OsString> {
        vec!["--remote-debugging-port=0".into()]
    }

    fn launch_args(
        &self,
        url: &str,
        profile: &Path,
        placement: Option<BrowserRect>,
    ) -> Vec<OsString> {
        // Each argument is its own element, so the URL needs no shell quoting.
        let mut a: Vec<OsString> = vec![format!("--app={url}").into()];
        let mut prof = OsString::from("--user-data-dir=");
        prof.push(profile.as_os_str());
        a.push(prof);
        if let Some(r) = placement {
            a.push(format!("--window-position={},{}", r.x, r.y).into());
            a.push(format!("--window-size={},{}", r.w, r.h).into());
        }
        a.push("--no-first-run".into());
        a.push("--no-default-browser-check".into());
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    #[derive(Default)]
    struct Fake {
        files: HashSet<PathBuf>,
        env: HashMap<String, String>,
        reg: Option<PathBuf>,
        path: Vec<PathBuf>,
        home: Option<PathBuf>,
    }
    impl Probe for Fake {
        fn exists(&self, p: &Path) -> bool {
            self.files.contains(p)
        }
        fn env(&self, n: &str) -> Option<String> {
            self.env.get(n).cloned()
        }
        fn app_paths_chrome(&self) -> Option<PathBuf> {
            self.reg.clone()
        }
        fn which(&self, n: &str) -> Option<PathBuf> {
            self.path.iter().find(|p| p.ends_with(n)).cloned()
        }
        fn home_dir(&self) -> Option<PathBuf> {
            self.home.clone()
        }
    }

    fn chrome(f: Fake, os: Os, ov: Option<&str>) -> Chrome {
        Chrome::with_probe(ov.map(PathBuf::from), Box::new(f)).with_os(os)
    }

    fn strs(v: Vec<OsString>) -> Vec<String> {
        v.into_iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn args_always_have_profile_and_no_placement_by_default() {
        let c = chrome(Fake::default(), Os::Linux, None);
        let a = strs(c.launch_args("http://127.0.0.1:9/d/tok/", Path::new("/p/profile"), None));
        assert_eq!(a[0], "--app=http://127.0.0.1:9/d/tok/");
        assert!(a.contains(&"--user-data-dir=/p/profile".to_string()));
        assert!(a.contains(&"--no-first-run".to_string()));
        assert!(a.contains(&"--no-default-browser-check".to_string()));
        assert!(!a.iter().any(|s| s.starts_with("--window-")));
    }

    #[test]
    fn placement_flags_only_when_given() {
        let c = chrome(Fake::default(), Os::Linux, None);
        let r = BrowserRect {
            x: -10,
            y: 20,
            w: 800,
            h: 600,
        };
        let a = strs(c.launch_args("http://x/", Path::new("/p"), Some(r)));
        assert!(a.contains(&"--window-position=-10,20".to_string()));
        assert!(a.contains(&"--window-size=800,600".to_string()));
    }

    #[test]
    fn url_with_spaces_stays_one_argument() {
        let c = chrome(Fake::default(), Os::Linux, None);
        let a = strs(c.launch_args("http://x/a b?c=1&d=2", Path::new("/p q"), None));
        assert_eq!(a[0], "--app=http://x/a b?c=1&d=2");
        assert_eq!(a[1], "--user-data-dir=/p q");
    }

    #[test]
    fn windows_prefers_registry_then_folders() {
        let reg = PathBuf::from(r"C:\R\chrome.exe");
        let mut f = Fake {
            reg: Some(reg.clone()),
            ..Default::default()
        };
        f.files.insert(reg.clone());
        assert_eq!(chrome(f, Os::Windows, None).discover(), Some(reg));

        let mut f = Fake {
            reg: Some(PathBuf::from(r"C:\gone.exe")),
            ..Default::default()
        };
        f.env.insert("ProgramFiles(x86)".into(), r"C:\PF86".into());
        let want = PathBuf::from(r"C:\PF86").join(r"Google\Chrome\Application\chrome.exe");
        f.files.insert(want.clone());
        assert_eq!(chrome(f, Os::Windows, None).discover(), Some(want));
    }

    #[test]
    fn macos_and_linux_discovery() {
        let mut f = Fake {
            home: Some(PathBuf::from("/h")),
            ..Default::default()
        };
        let want = PathBuf::from("/h/Applications/Google Chrome.app/Contents/MacOS/Google Chrome");
        f.files.insert(want.clone());
        assert_eq!(chrome(f, Os::MacOs, None).discover(), Some(want));

        let f = Fake {
            path: vec![PathBuf::from("/usr/bin/google-chrome-stable")],
            ..Default::default()
        };
        assert_eq!(
            chrome(f, Os::Linux, None).discover(),
            Some(PathBuf::from("/usr/bin/google-chrome-stable"))
        );
    }

    #[test]
    fn override_wins_and_missing_override_is_not_found() {
        let mut f = Fake::default();
        f.files.insert(PathBuf::from("/o/chrome"));
        assert_eq!(
            chrome(f, Os::Linux, Some("/o/chrome")).discover(),
            Some("/o/chrome".into())
        );
        let f = Fake {
            path: vec![PathBuf::from("/usr/bin/google-chrome")],
            ..Default::default()
        };
        assert_eq!(chrome(f, Os::Linux, Some("/nope")).discover(), None);
    }

    #[test]
    fn parses_reg_output() {
        let s = "\r\nHKEY_LOCAL_MACHINE\\SOFTWARE\\X\r\n    (Default)    REG_SZ    C:\\A B\\chrome.exe\r\n";
        assert_eq!(
            parse_reg_default(s),
            Some(PathBuf::from(r"C:\A B\chrome.exe"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn real_windows_discovery_finds_chrome() {
        let found = Chrome::new(None).discover();
        eprintln!("{found:?}");
        assert!(found.is_some());
    }
}
