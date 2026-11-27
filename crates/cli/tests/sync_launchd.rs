//! `tierbridge sync install-launchd` / `uninstall-launchd` against a scratch `HOME`: the real
//! `~/Library/LaunchAgents` is never touched and `launchctl` is never called (`--dry-run`,
//! `--no-load`, `--no-unload`).

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_tierbridge");
const PLIST: &str = "Library/LaunchAgents/dev.blyzer.llm-tierbridge.sync.plist";
const WRAPPER: &str = "Library/Application Support/llm-tierbridge/sync-launchd.sh";

struct Home(PathBuf);

impl Home {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("tierbridge-launchd-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("home")).unwrap();
        Self(dir)
    }

    fn home(&self) -> PathBuf {
        self.0.join("home")
    }

    /// A data repo directory the installer accepts (it only looks for the state file).
    fn data_repo(&self) -> PathBuf {
        let d = self.0.join("data repo");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("sync-state.json"), "{}").unwrap();
        d
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .env("HOME", self.home())
            .output()
            .unwrap()
    }

    fn files_under_home(&self) -> Vec<String> {
        fn walk(dir: &Path, out: &mut Vec<String>) {
            for e in fs::read_dir(dir).unwrap().flatten() {
                if e.path().is_dir() {
                    walk(&e.path(), out);
                } else {
                    out.push(e.path().display().to_string());
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.home(), &mut out);
        out
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn dry_run_prints_the_plist_and_wrapper_and_writes_nothing() {
    let h = Home::new("dry");
    let data = h.data_repo();
    let out = h.run(&[
        "sync",
        "install-launchd",
        "--dry-run",
        "--data-repo",
        data.to_str().unwrap(),
        "--hour",
        "7",
        "--keychain-var",
        "EASYCLIPROXY_API_KEY",
        "--keychain-var",
        "OTHER_KEY",
        "--bin",
        "/opt/tb/tierbridge",
    ]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    for needle in [
        "dev.blyzer.llm-tierbridge.sync",
        "<key>Hour</key>\n    <integer>7</integer>",
        "Library/Logs/llm-tierbridge/sync.err.log",
        "load_keychain 'EASYCLIPROXY_API_KEY'",
        "load_keychain 'OTHER_KEY'",
        "security}\" find-generic-password -s llm-tierbridge -a \"$1\" -w",
        "exec '/opt/tb/tierbridge' sync run --data-repo",
        "data repo'", // the path with a space is quoted
        "would run: launchctl bootstrap",
    ] {
        assert!(stdout.contains(needle), "{needle} missing in\n{stdout}");
    }
    assert!(
        h.files_under_home().is_empty(),
        "{:?}",
        h.files_under_home()
    );
}

#[test]
fn install_without_load_writes_both_files_and_uninstall_removes_them() {
    let h = Home::new("install");
    let data = h.data_repo();
    let out = h.run(&[
        "sync",
        "install-launchd",
        "--no-load",
        "--data-repo",
        data.to_str().unwrap(),
        "--bin",
        "/opt/tb/tierbridge",
    ]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let plist = h.home().join(PLIST);
    let wrapper = h.home().join(WRAPPER);
    assert!(plist.is_file() && wrapper.is_file());
    assert_eq!(
        fs::metadata(&wrapper).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert!(h.home().join("Library/Logs/llm-tierbridge").is_dir());
    let plist_text = fs::read_to_string(&plist).unwrap();
    assert!(
        plist_text.contains("<integer>6</integer>"),
        "default hour is 6"
    );
    assert!(plist_text.contains(&wrapper.display().to_string()));
    assert!(
        fs::read_to_string(&wrapper)
            .unwrap()
            .contains("load_keychain 'EASYCLIPROXY_API_KEY'")
    );

    // Uninstall: a dry run first, then the real thing (no launchctl).
    let dry = h.run(&["sync", "uninstall-launchd", "--dry-run"]);
    assert!(dry.status.success());
    assert!(text(&dry.stdout).contains("would remove"));
    assert!(plist.exists() && wrapper.exists());
    let out = h.run(&["sync", "uninstall-launchd", "--no-unload"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(!plist.exists() && !wrapper.exists());
    let again = h.run(&["sync", "uninstall-launchd", "--no-unload"]);
    assert!(text(&again.stdout).contains("nothing installed"));
}

#[test]
fn bad_arguments_are_refused() {
    let h = Home::new("bad");
    let data = h.data_repo();
    let d = data.to_str().unwrap();
    for (args, message) in [
        (vec!["--keychain-var", "A;rm -rf"], "is not a variable name"),
        (vec!["--hour", "24"], "24"),
    ] {
        let mut full = vec!["sync", "install-launchd", "--dry-run", "--data-repo", d];
        full.extend(args);
        let out = h.run(&full);
        assert!(!out.status.success());
        assert!(text(&out.stderr).contains(message), "{}", text(&out.stderr));
    }
    // a real install needs an initialized data repo
    let out = h.run(&[
        "sync",
        "install-launchd",
        "--no-load",
        "--data-repo",
        "/nonexistent/x",
    ]);
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("not a data repo"));
    assert!(h.files_under_home().is_empty());
}

/// Runs the generated wrapper with a fake `security` and a fake `tierbridge`: the secrets reach
/// the child's environment, a missing item only warns, and nothing is echoed.
#[test]
fn the_wrapper_loads_keychain_items_into_the_job_environment() {
    let h = Home::new("wrapper");
    let data = h.data_repo();
    let record = h.0.join("record");
    fs::create_dir_all(&record).unwrap();
    let script = |name: &str, body: &str| {
        let p = h.0.join(name);
        fs::write(&p, body).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        p
    };
    let fake_security = script(
        "security",
        "#!/bin/sh\n# find-generic-password -s SERVICE -a ACCOUNT -w\n\
         echo \"$@\" >> \"$FAKE_RECORD/security-args\"\n\
         if [ \"$4\" = \"-a\" ] && [ \"$5\" = \"HAVE_KEY\" ]; then echo 'secret value with spaces'; exit 0; fi\n\
         exit 44\n",
    );
    let fake_tb = script(
        "tierbridge",
        "#!/bin/sh\necho \"$@\" > \"$FAKE_RECORD/args\"\nprintf '%s' \"$HAVE_KEY\" > \"$FAKE_RECORD/have\"\n\
         printf '%s' \"${MISSING_KEY-unset}\" > \"$FAKE_RECORD/missing\"\n",
    );
    let out = h.run(&[
        "sync",
        "install-launchd",
        "--no-load",
        "--data-repo",
        data.to_str().unwrap(),
        "--bin",
        fake_tb.to_str().unwrap(),
        "--keychain-var",
        "HAVE_KEY",
        "--keychain-var",
        "MISSING_KEY",
    ]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let run = Command::new("/bin/sh")
        .arg(h.home().join(WRAPPER))
        .env("TIERBRIDGE_SECURITY", &fake_security)
        .env("FAKE_RECORD", &record)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", text(&run.stderr));
    assert_eq!(
        fs::read_to_string(record.join("have")).unwrap(),
        "secret value with spaces"
    );
    assert_eq!(fs::read_to_string(record.join("missing")).unwrap(), "unset");
    let args = fs::read_to_string(record.join("args")).unwrap();
    assert!(args.starts_with("sync run --data-repo "), "{args}");
    let stderr = text(&run.stderr);
    assert!(
        stderr.contains("no Keychain item llm-tierbridge/MISSING_KEY"),
        "{stderr}"
    );
    assert!(!stderr.contains("secret value") && !text(&run.stdout).contains("secret value"));
    let asked = fs::read_to_string(record.join("security-args")).unwrap();
    assert!(
        asked.contains("find-generic-password -s llm-tierbridge -a HAVE_KEY -w"),
        "{asked}"
    );
}
