//! Install a systemd unit so trck starts on boot.

use std::fs;
use std::io::{self, ErrorKind};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, ExitCode};

use anyhow::{Context as _, Result, anyhow, bail};
use nix::unistd::Uid;

const UNIT: &str = include_str!("../packaging/trck.service");
const UNIT_PATH: &str = "/etc/systemd/system/trck.service";
const BIN_PATH: &str = "/usr/local/bin/trck";
const WORK_DIR: &str = "/var/lib/trck";

pub fn run() -> ExitCode {
    match install() {
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

fn install() -> Result<()> {
    if !Uid::effective().is_root() {
        bail!("trck install-service needs root");
    }
    let exe = std::env::current_exe().context("cannot resolve this binary")?;
    fs::create_dir_all(WORK_DIR).with_context(|| format!("cannot create {WORK_DIR}"))?;
    place_bin(&exe, Path::new(BIN_PATH))?;
    write_unit(Path::new(UNIT_PATH), UNIT)?;
    run_host(&["systemctl", "daemon-reload"])?;
    run_host(&["systemctl", "enable", "--now", "trck"])?;
    Ok(())
}

fn place_bin(src: &Path, dest: &Path) -> Result<()> {
    if same_file(src, dest) {
        return Ok(());
    }
    fs::copy(src, dest).map_err(|err| write_fail(dest, err))?;
    let mut perm = fs::metadata(dest)
        .map_err(|err| write_fail(dest, err))?
        .permissions();
    perm.set_mode(perm.mode() | 0o755);
    fs::set_permissions(dest, perm).map_err(|err| write_fail(dest, err))?;
    Ok(())
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn write_unit(path: &Path, text: &str) -> Result<()> {
    if fs::read_to_string(path).is_ok_and(|got| got == text) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    fs::write(path, text).map_err(|err| write_fail(path, err))
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
        assert!(UNIT.contains("WantedBy=multi-user.target"));
        assert!(UNIT.contains("ExecStart=/usr/local/bin/trck"));
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
        write_unit(&path, UNIT).unwrap();
        write_unit(&path, UNIT).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), UNIT);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn place_bin_copies_and_skips_self() {
        let dir = temp_dir();
        let src = dir.join("trck");
        let dest = dir.join("installed");
        fs::write(&src, b"bin").unwrap();
        place_bin(&src, &dest).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"bin");
        place_bin(&dest, &dest).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"bin");
        let _ = fs::remove_dir_all(&dir);
    }
}
