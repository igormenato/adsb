//! Install a systemd unit so trck starts on boot.

use std::fs;
use std::io::{self, ErrorKind};
use std::path::Path;
use std::process::{Command, ExitCode};

use anyhow::{Context as _, Result, anyhow, bail};
use nix::unistd::Uid;

use super::update;

const UNIT: &str = include_str!("../packaging/trck.service");
pub(crate) const UNIT_PATH: &str = "/etc/systemd/system/trck.service";
pub(crate) const BIN_PATH: &str = "/usr/local/bin/trck";
const WORK_DIR: &str = "/var/lib/trck";

pub fn run(hex: Option<&str>) -> ExitCode {
    match install(hex) {
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

fn unit_text(hex: Option<&str>) -> String {
    let Some(addr) = hex else {
        return UNIT.to_string();
    };
    UNIT.replace(
        "After=local-fs.target\n",
        "After=local-fs.target\nAfter=readsb.service\nWants=readsb.service\n",
    )
    .replace(
        "ExecStart=/usr/local/bin/trck --allow-power-commands\n",
        &format!("ExecStart=/usr/local/bin/trck --allow-power-commands --hex {addr}\n"),
    )
}

fn install(hex: Option<&str>) -> Result<()> {
    if !Uid::effective().is_root() {
        bail!("trck install-service needs root");
    }
    let exe = std::env::current_exe().context("cannot resolve this binary")?;
    fs::create_dir_all(WORK_DIR).with_context(|| format!("cannot create {WORK_DIR}"))?;
    let had_unit = Path::new(UNIT_PATH).exists();
    let bin_changed = place_bin(&exe, Path::new(BIN_PATH))?;
    let unit = unit_text(hex);
    let unit_changed = write_unit(Path::new(UNIT_PATH), &unit)?;
    run_host(&["systemctl", "daemon-reload"])?;
    run_host(&["systemctl", "enable", "--now", "trck"])?;
    if had_unit && (bin_changed || unit_changed) {
        run_host(&["systemctl", "restart", "trck"])?;
    }
    Ok(())
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

pub(crate) fn run_host(args: &[&str]) -> Result<()> {
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
    fn default_unit_is_unchanged() {
        assert_eq!(unit_text(None), UNIT);
    }

    #[test]
    fn hex_unit_starts_trck_on_readsb_feed() {
        let text = unit_text(Some("127.0.0.1:30002"));
        assert!(text.contains(
            "ExecStart=/usr/local/bin/trck --allow-power-commands --hex 127.0.0.1:30002"
        ));
        assert!(text.contains("After=readsb.service"));
        assert!(text.contains("Wants=readsb.service"));
        assert!(text.contains("After=local-fs.target"));
        assert!(!text.contains("ExecStart=/usr/local/bin/trck --allow-power-commands\n"));
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
    fn write_unit_is_idempotent() {
        let dir = temp_dir();
        let path = dir.join("trck.service");
        assert!(write_unit(&path, UNIT).unwrap());
        assert!(!write_unit(&path, UNIT).unwrap());
        assert!(write_unit(&path, "[Unit]\n").unwrap());
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
