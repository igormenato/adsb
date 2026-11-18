//! Install a systemd unit so trck starts on boot.

use std::fs;
use std::io::{self, ErrorKind};
use std::path::Path;
use std::process::{Command, ExitCode};

use anyhow::{Context as _, Result, anyhow, bail};
use nix::unistd::Uid;

use super::update;

const UNIT: &str = include_str!("../packaging/trck.service");
const UNIT_PATH: &str = "/etc/systemd/system/trck.service";
const BIN_PATH: &str = "/usr/local/bin/trck";
const WORK_DIR: &str = "/var/lib/trck";
const ALLOW_POWER: &str = "--allow-power-commands";

pub fn run(args: &[String]) -> ExitCode {
    if let Err(err) = super::parse_run_args(args) {
        let _ = err.print();
        return ExitCode::from(1);
    }
    match install(args) {
        Ok(()) => {
            println!("enabled trck.service");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("{err:#}");
            ExitCode::from(1)
        }
    }
}

fn install(args: &[String]) -> Result<()> {
    if !Uid::effective().is_root() {
        bail!("trck install-service needs root");
    }
    let exe = std::env::current_exe().context("cannot resolve this binary")?;
    fs::create_dir_all(WORK_DIR).with_context(|| format!("cannot create {WORK_DIR}"))?;
    let had_unit = Path::new(UNIT_PATH).exists();
    let bin_changed = place_bin(&exe, Path::new(BIN_PATH))?;
    let unit = render_unit(args);
    let unit_changed = write_unit(Path::new(UNIT_PATH), &unit)?;
    run_host(&["systemctl", "daemon-reload"])?;
    run_host(&["systemctl", "enable", "--now", "trck"])?;
    if had_unit && (bin_changed || unit_changed) {
        run_host(&["systemctl", "restart", "trck"])?;
    }
    Ok(())
}

fn render_unit(args: &[String]) -> String {
    UNIT.lines()
        .map(|line| {
            if line.starts_with("ExecStart=") {
                exec_start(args)
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn exec_start(args: &[String]) -> String {
    let mut parts = vec![BIN_PATH.to_string(), ALLOW_POWER.to_string()];
    for arg in args {
        if arg == ALLOW_POWER {
            continue;
        }
        parts.push(systemd_quote(arg));
    }
    format!("ExecStart={}", parts.join(" "))
}

fn systemd_quote(arg: &str) -> String {
    let plain = arg.bytes().all(|b| {
        b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'-' | b'_' | b'.' | b'/' | b':' | b'@' | b'+' | b'=' | b','
            )
    });
    if plain && !arg.is_empty() {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    for c in arg.chars() {
        match c {
            '\\' | '"' | '$' | '`' => {
                out.push('\\');
                out.push(c);
            }
            '%' => out.push_str("%%"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

fn place_bin(src: &Path, dest: &Path) -> Result<bool> {
    if same_file(src, dest) {
        return Ok(false);
    }
    update::replace_file(src, dest)?;
    Ok(true)
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn write_unit(path: &Path, text: &str) -> Result<bool> {
    if fs::read_to_string(path).is_ok_and(|got| got == text) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    fs::write(path, text).map_err(|err| write_fail(path, err))?;
    Ok(true)
}

fn run_host(args: &[&str]) -> Result<()> {
    let (cmd, rest) = args.split_first().expect("command");
    let status = Command::new(cmd)
        .args(rest)
        .status()
        .with_context(|| format!("cannot run {cmd}"))?;
    if status.success() {
        Ok(())
    } else {
        bail!("{cmd} failed: {status}")
    }
}

fn write_fail(path: &Path, err: io::Error) -> anyhow::Error {
    if err.kind() == ErrorKind::PermissionDenied {
        anyhow!(
            "cannot write {}: permission denied; run sudo trck install-service",
            path.display()
        )
    } else {
        anyhow!("cannot write {}: {err}", path.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "trck-service-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn unit_is_simple_always_restart() {
        assert!(UNIT.contains("Type=simple"));
        assert!(UNIT.contains("Restart=always"));
        assert!(UNIT.contains("RestartSec=1"));
        assert!(UNIT.contains("StartLimitIntervalSec=0"));
        assert!(UNIT.contains("WantedBy=multi-user.target"));
        assert!(UNIT.contains("ExecStart=/usr/local/bin/trck --allow-power-commands"));
        assert!(UNIT.contains("ProtectHome=yes"));
        assert!(UNIT.contains("PrivateTmp=yes"));
        assert!(!UNIT.contains("User="));
        assert!(!UNIT.contains("WatchdogSec"));
        assert!(!UNIT.contains("ProtectSystem"));
    }

    #[test]
    fn exec_start_always_allows_power_and_quotes() {
        let none = exec_start(&[]);
        assert_eq!(none, "ExecStart=/usr/local/bin/trck --allow-power-commands");
        let args = [
            "--ppm".into(),
            "-8".into(),
            "--gain".into(),
            "19.7".into(),
            ALLOW_POWER.into(),
            "a b".into(),
            "say\"hi".into(),
            "%n".into(),
            "50%".into(),
        ];
        let line = exec_start(&args);
        assert!(line.starts_with("ExecStart=/usr/local/bin/trck --allow-power-commands "));
        assert_eq!(line.matches(ALLOW_POWER).count(), 1);
        assert!(line.contains(" --ppm -8 --gain 19.7 "));
        assert!(line.contains("\"a b\""));
        assert!(line.contains("\"say\\\"hi\""));
        assert_eq!(systemd_quote("%n"), "\"%%n\"");
        assert_eq!(systemd_quote("50%"), "\"50%%\"");
        assert!(line.contains("\"%%n\""));
        assert!(line.contains("\"50%%\""));
        let again = render_unit(&["--gain".into(), "12".into()]);
        assert_ne!(render_unit(&[]), again);
        assert!(again.contains("StartLimitIntervalSec=0"));
        assert!(again.contains("--gain 12"));
    }

    #[test]
    fn write_unit_is_idempotent() {
        let dir = temp_dir();
        let path = dir.join("trck.service");
        let text = render_unit(&[]);
        assert!(write_unit(&path, &text).unwrap());
        assert!(!write_unit(&path, &text).unwrap());
        assert!(write_unit(&path, &render_unit(&["--ppm".into(), "-8".into()])).unwrap());
        let got = fs::read_to_string(&path).unwrap();
        assert!(got.contains("--ppm -8"));
        assert!(got.contains("StartLimitIntervalSec=0"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn place_bin_copies_and_skips_self() {
        let dir = temp_dir();
        let src = dir.join("trck");
        let dest = dir.join("installed");
        fs::write(&src, b"bin").unwrap();
        assert!(place_bin(&src, &dest).unwrap());
        assert_eq!(fs::read(&dest).unwrap(), b"bin");
        assert!(!place_bin(&dest, &dest).unwrap());
        fs::write(&src, b"new").unwrap();
        assert!(place_bin(&src, &dest).unwrap());
        assert_eq!(fs::read(&dest).unwrap(), b"new");
        assert!(!dir.join("installed.new").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
