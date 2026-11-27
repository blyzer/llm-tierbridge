//! `tierbridge apply omp` on a scratch `models.yml`: dry run, backup, idempotence, revert.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const MODELS: &str = "\
# comments survive
providers:
  easycliproxy:
    baseUrl: http://127.0.0.1:8317/v1
    apiKey: sk-not-a-real-key
    discovery:
      type: openai-models-list

  dsh:
    baseUrl: http://127.0.0.1:8326/v1
";

const OVERRIDES: &str = "\
providers:
  easycliproxy:
    modelOverrides:
      a/b:
        contextWindow: 4096
";

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("tierbridge-apply-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("models.yml"), MODELS).unwrap();
        fs::write(dir.join("overrides.yml"), OVERRIDES).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(dir.join("models.yml"), fs::Permissions::from_mode(0o600)).unwrap();
        }
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn apply(&self, extra: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_tierbridge"))
            .args(["apply", "omp", "--models-yml"])
            .arg(self.path("models.yml"))
            .args(extra)
            .output()
            .unwrap()
    }

    fn apply_overrides(&self, extra: &[&str]) -> Output {
        let overrides = self.path("overrides.yml");
        let mut args = vec!["--overrides", overrides.to_str().unwrap()];
        args.extend(extra);
        self.apply(&args)
    }

    fn backups(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = fs::read_dir(&self.0)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                p.to_string_lossy().contains(".tierbridge-")
                    && p.to_string_lossy().ends_with(".bak")
            })
            .collect();
        v.sort();
        v
    }

    fn models(&self) -> String {
        fs::read_to_string(self.path("models.yml")).unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn ok(out: &Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn dry_run_writes_nothing() {
    let s = Scratch::new("dry");
    let stdout = ok(&s.apply_overrides(&["--dry-run"]));
    assert!(stdout.contains("easycliproxy: block of"), "{stdout}");
    assert!(stdout.contains("dry run: nothing written"), "{stdout}");
    assert_eq!(s.models(), MODELS);
    assert!(s.backups().is_empty());
}

#[test]
fn apply_backs_up_is_idempotent_and_revert_restores_the_bytes() {
    let s = Scratch::new("cycle");
    let first = ok(&s.apply_overrides(&[]));
    assert!(first.contains("wrote "), "{first}");
    let applied = s.models();
    assert!(applied.contains("# >>> tierbridge-managed modelOverrides (do not edit) >>>"));
    assert!(applied.contains("      a/b:\n        contextWindow: 4096"));
    let backups = s.backups();
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert_eq!(
        fs::read_to_string(&backups[0]).unwrap(),
        MODELS,
        "backup is the original"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        for p in [s.path("models.yml"), backups[0].clone()] {
            let mode = fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{} keeps the secret file's mode", p.display());
        }
    }

    let again = ok(&s.apply_overrides(&[]));
    assert!(again.contains("already up to date"), "{again}");
    assert_eq!(s.models(), applied, "same bytes");
    assert_eq!(s.backups().len(), 1, "no backup for a no-op");

    ok(&s.apply(&["--revert", "--dry-run"]));
    assert_eq!(s.models(), applied);
    ok(&s.apply(&["--revert"]));
    assert_eq!(s.models(), MODELS);
    assert!(ok(&s.apply(&["--revert"])).contains("already up to date"));
    assert!(Path::new(&s.backups()[0]).exists());
}

#[test]
fn refusals_leave_the_file_alone_and_exit_nonzero() {
    let s = Scratch::new("refuse");
    let own = MODELS.replace(
        "    discovery:\n",
        "    modelOverrides:\n      x:\n        name: mine\n    discovery:\n",
    );
    fs::write(s.path("models.yml"), &own).unwrap();
    let out = s.apply_overrides(&[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("already has a modelOverrides"));
    assert_eq!(s.models(), own);
    assert!(s.backups().is_empty());

    // --overrides and --revert are exclusive; one of them is required
    assert!(!s.apply(&[]).status.success());
    let both = s.path("overrides.yml");
    assert!(
        !s.apply(&["--revert", "--overrides", both.to_str().unwrap()])
            .status
            .success()
    );
}
