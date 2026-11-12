//! Host probe for RTL-SDR, UART, and the CSV backup log.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use super::sys;

const UART_PATH: &str = "/dev/serial0";
const CSV_PATH: &str = "aircraft.csv";
const USB_SYS: &str = "/sys/bus/usb/devices";
const CONFIG_PATHS: &[&str] = &["/boot/firmware/config.txt", "/boot/config.txt"];
/// Realtek VID/PIDs `rs-rtl` (nusb) opens. Clones with other IDs stay dark.
const RTL_VID: u16 = 0x0bda;
const RTL_PIDS: &[u16] = &[0x2832, 0x2838];
/// Unix seconds before 2020-01-01. A Pi with no RTC and no NTP sits near 1970.
const CLOCK_OK_AFTER_S: u64 = 1_577_836_800;
const DVB_DRIVERS: &[&str] = &[
    "dvb_usb_rtl28xxu",
    "dvb_usb_rtl2832u",
    "rtl2832",
    "rtl2832_sdr",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    Pass,
    Fail,
    Warn,
}

#[derive(Debug, PartialEq, Eq)]
struct Check {
    name: &'static str,
    status: Status,
    detail: String,
}

impl Check {
    fn pass(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Pass,
            detail: detail.into(),
        }
    }

    fn fail(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Fail,
            detail: detail.into(),
        }
    }

    fn warn(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Warn,
            detail: detail.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct UsbDev {
    vid: u16,
    pid: u16,
    bus: u32,
    dev: u32,
    driver: Option<String>,
}

impl UsbDev {
    fn usbfs(&self) -> PathBuf {
        PathBuf::from(format!("/dev/bus/usb/{:03}/{:03}", self.bus, self.dev))
    }

    fn id(&self) -> String {
        format!("{:04x}:{:04x}", self.vid, self.pid)
    }
}

struct ConsoleProbe {
    cmdline: String,
    stdout_path: Option<String>,
    model: Option<String>,
    config: Option<(String, String)>,
}

pub fn run() -> ExitCode {
    let checks = probe();
    let mut failed = false;
    for check in &checks {
        println!("{}", format_line(check));
        if matches!(check.status, Status::Fail) {
            failed = true;
        }
    }
    if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn probe() -> Vec<Check> {
    let usb = scan_usb();
    vec![
        check_rtlsdr(&usb),
        check_uart(Path::new(UART_PATH)),
        check_uart_console(&live_console()),
        check_csv(Path::new(CSV_PATH)),
        check_usb(&usb),
        check_clock(SystemTime::now()),
    ]
}

fn format_line(check: &Check) -> String {
    let tag = match check.status {
        Status::Pass => "ok",
        Status::Fail => "fail",
        Status::Warn => "warn",
    };
    format!("{tag} {}: {}", check.name, check.detail)
}

fn is_rtl(vid: u16, pid: u16) -> bool {
    vid == RTL_VID && RTL_PIDS.contains(&pid)
}

fn check_rtlsdr(devs: &[UsbDev]) -> Check {
    match devs.iter().find(|dev| is_rtl(dev.vid, dev.pid)) {
        Some(dev) => Check::pass(
            "rtlsdr",
            format!("{} bus {} dev {}", dev.id(), dev.bus, dev.dev),
        ),
        None => Check::fail(
            "rtlsdr",
            "no RTL-SDR USB device; plug in a Realtek 0bda:2832 or 0bda:2838 dongle",
        ),
    }
}

fn check_uart(path: &Path) -> Check {
    if !sys::present(path) {
        return Check::fail(
            "uart",
            format!(
                "{} missing; enable UART in raspi-config so {UART_PATH} exists",
                path.display()
            ),
        );
    }
    if !sys::writable(path) {
        return Check::fail(
            "uart",
            format!(
                "{} not writable; sudo usermod -aG dialout $USER and log in again",
                path.display()
            ),
        );
    }
    Check::pass("uart", format!("{} writable", path.display()))
}

fn check_uart_console(probe: &ConsoleProbe) -> Check {
    if !looks_like_pi(probe) {
        return Check::pass("uart-console", "no serial console");
    }
    if let Some(token) = serial_console_token(&probe.cmdline) {
        return Check::fail(
            "uart-console",
            format!("{token} owns the UART; remove it from cmdline.txt (serial login No)"),
        );
    }
    if let Some(path) = probe.stdout_path.as_deref()
        && stdout_is_serial(path)
    {
        return Check::fail(
            "uart-console",
            format!("device-tree stdout-path {path} is serial; disable the serial console"),
        );
    }
    if let Some((config_path, text)) = probe.config.as_ref() {
        let parsed = parse_config_txt(text);
        if parsed.enable_uart != Some(true) {
            return Check::fail(
                "uart-console",
                format!("set enable_uart=1 in {config_path}"),
            );
        }
        if is_zero_w(probe.model.as_deref().unwrap_or("")) && !parsed.disable_bt {
            return Check::fail(
                "uart-console",
                format!(
                    "add dtoverlay=disable-bt to {config_path} so Bluetooth does not own the UART"
                ),
            );
        }
        return Check::pass(
            "uart-console",
            format!("enable_uart=1 in {config_path}, no serial console"),
        );
    }
    Check::pass("uart-console", "no serial console")
}

fn looks_like_pi(probe: &ConsoleProbe) -> bool {
    if let Some(model) = probe.model.as_deref()
        && model.to_ascii_lowercase().contains("raspberry pi")
    {
        return true;
    }
    probe.config.is_some()
}

fn check_csv(path: &Path) -> Check {
    if path.is_dir() {
        return Check::fail(
            "csv",
            format!("{} is a directory; use a file in cwd", path.display()),
        );
    }
    let ok = if sys::present(path) {
        sys::writable(path)
    } else {
        let parent = match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        sys::writable(parent)
    };
    if ok {
        Check::pass("csv", format!("{} writable", path.display()))
    } else {
        Check::fail(
            "csv",
            format!(
                "{} not writable; check cwd permissions and disk space",
                path.display()
            ),
        )
    }
}

fn check_usb(devs: &[UsbDev]) -> Check {
    match devs.iter().find(|dev| is_rtl(dev.vid, dev.pid)) {
        Some(dev) => {
            if let Some(driver) = dev.driver.as_deref()
                && DVB_DRIVERS.contains(&driver)
            {
                return Check::fail(
                    "usb",
                    format!(
                        "kernel driver {driver} has the dongle; sudo rmmod {driver} so nusb can open it"
                    ),
                );
            }
            let node = dev.usbfs();
            if !sys::writable(&node) {
                return Check::fail(
                    "usb",
                    format!(
                        "{} not writable; sudo usermod -aG plugdev $USER and replug",
                        node.display()
                    ),
                );
            }
            Check::pass("usb", format!("{} writable", node.display()))
        }
        None => {
            if in_group("plugdev") {
                Check::pass("usb", "user is in plugdev")
            } else {
                Check::fail(
                    "usb",
                    "user not in plugdev; sudo usermod -aG plugdev $USER and log in again",
                )
            }
        }
    }
}

fn check_clock(now: SystemTime) -> Check {
    match now.duration_since(UNIX_EPOCH) {
        Ok(d) if d.as_secs() >= CLOCK_OK_AFTER_S => {
            Check::pass("clock", format!("unix {}", d.as_secs()))
        }
        Ok(d) => Check::warn(
            "clock",
            format!(
                "unix {} looks unset (1970); enable NTP or set the date",
                d.as_secs()
            ),
        ),
        Err(_) => Check::warn("clock", "time is before 1970; enable NTP or set the date"),
    }
}

fn serial_console_token(cmdline: &str) -> Option<&str> {
    cmdline.split_whitespace().find(|tok| {
        let Some(rest) = tok.strip_prefix("console=") else {
            return false;
        };
        let name = rest.split(',').next().unwrap_or("");
        matches!(name, "serial0" | "serial1" | "ttyAMA0" | "ttyS0")
    })
}

fn stdout_is_serial(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains("serial") || lower.contains("uart")
}

fn is_zero_w(model: &str) -> bool {
    model.to_ascii_lowercase().contains("pi zero 2 w")
}

struct FirmwareConfig {
    enable_uart: Option<bool>,
    disable_bt: bool,
}

fn parse_config_txt(text: &str) -> FirmwareConfig {
    let mut enable_uart = None;
    let mut disable_bt = false;
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() || line.starts_with('[') {
            continue;
        }
        let compact: String = line
            .chars()
            .filter(|c| !c.is_ascii_whitespace())
            .collect::<String>()
            .to_ascii_lowercase();
        if let Some(value) = compact.strip_prefix("enable_uart=") {
            enable_uart = Some(value == "1");
        }
        if let Some(overlay) = compact.strip_prefix("dtoverlay=")
            && (overlay.starts_with("disable-bt")
                || overlay.starts_with("miniuart-bt")
                || overlay.starts_with("pi3-disable-bt"))
        {
            disable_bt = true;
        }
    }
    FirmwareConfig {
        enable_uart,
        disable_bt,
    }
}

fn live_console() -> ConsoleProbe {
    ConsoleProbe {
        cmdline: fs::read_to_string("/proc/cmdline").unwrap_or_default(),
        stdout_path: read_dt(Path::new("/proc/device-tree/chosen/stdout-path")),
        model: read_dt(Path::new("/proc/device-tree/model")),
        config: read_first_config(),
    }
}

fn read_dt(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8(bytes[..end].to_vec())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn read_first_config() -> Option<(String, String)> {
    for path in CONFIG_PATHS {
        if let Ok(text) = fs::read_to_string(path) {
            return Some(((*path).to_string(), text));
        }
    }
    None
}

fn scan_usb() -> Vec<UsbDev> {
    let Ok(entries) = fs::read_dir(USB_SYS) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.contains(':') {
            continue;
        }
        let Some(vid) = read_hex_u16(&path.join("idVendor")) else {
            continue;
        };
        let Some(pid) = read_hex_u16(&path.join("idProduct")) else {
            continue;
        };
        if !is_rtl(vid, pid) {
            continue;
        }
        let Some(bus) = read_u32(&path.join("busnum")) else {
            continue;
        };
        let Some(dev) = read_u32(&path.join("devnum")) else {
            continue;
        };
        out.push(UsbDev {
            vid,
            pid,
            bus,
            dev,
            driver: interface_driver(&path),
        });
    }
    out
}

fn interface_driver(dev_dir: &Path) -> Option<String> {
    for entry in fs::read_dir(dev_dir).ok()?.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().contains(':') {
            continue;
        }
        let link = fs::read_link(entry.path().join("driver")).ok()?;
        return link.file_name().map(|s| s.to_string_lossy().into_owned());
    }
    None
}

fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn read_hex_u16(path: &Path) -> Option<u16> {
    u16::from_str_radix(read_trimmed(path)?.trim_start_matches("0x"), 16).ok()
}

fn read_u32(path: &Path) -> Option<u32> {
    read_trimmed(path)?.parse().ok()
}

fn in_group(name: &str) -> bool {
    let Some(gid) = group_gid(name) else {
        return false;
    };
    sys::groups().contains(&gid)
}

fn group_gid(name: &str) -> Option<u32> {
    for line in fs::read_to_string("/etc/group").ok()?.lines() {
        let mut parts = line.split(':');
        let group = parts.next()?;
        if group != name {
            continue;
        }
        let _passwd = parts.next()?;
        return parts.next()?.parse().ok();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn rtl() -> UsbDev {
        UsbDev {
            vid: RTL_VID,
            pid: 0x2838,
            bus: 1,
            dev: 4,
            driver: None,
        }
    }

    #[test]
    fn format_line_starts_with_status() {
        assert_eq!(
            format_line(&Check::pass("csv", "aircraft.csv writable")),
            "ok csv: aircraft.csv writable"
        );
        assert_eq!(
            format_line(&Check::fail("uart", "/dev/serial0 missing; enable UART")),
            "fail uart: /dev/serial0 missing; enable UART"
        );
        assert_eq!(
            format_line(&Check::warn(
                "clock",
                "unix 0 looks unset (1970); enable NTP"
            )),
            "warn clock: unix 0 looks unset (1970); enable NTP"
        );
    }

    #[test]
    fn rs_rtl_ids_are_the_only_match() {
        assert!(is_rtl(0x0bda, 0x2838));
        assert!(is_rtl(0x0bda, 0x2832));
        assert!(!is_rtl(0x0bda, 0x2839));
        assert!(!is_rtl(0x1d6b, 0x0002));
    }

    #[test]
    fn rtlsdr_fail_names_the_dongle() {
        let check = check_rtlsdr(&[]);
        assert_eq!(check.status, Status::Fail);
        assert!(check.detail.contains("0bda:2838"), "{}", check.detail);
        let check = check_rtlsdr(&[rtl()]);
        assert_eq!(check.status, Status::Pass);
        assert!(check.detail.contains("0bda:2838"), "{}", check.detail);
    }

    #[test]
    fn usb_reports_dvb_driver() {
        let mut dev = rtl();
        dev.driver = Some("dvb_usb_rtl28xxu".into());
        let check = check_usb(&[dev]);
        assert_eq!(check.status, Status::Fail);
        assert!(check.detail.contains("nusb"), "{}", check.detail);
    }

    #[test]
    fn cmdline_serial_console_is_a_steal() {
        assert_eq!(serial_console_token("console=tty1"), None);
        assert_eq!(
            serial_console_token("coherent_pool=1M console=serial0,115200 console=tty1"),
            Some("console=serial0,115200")
        );
        assert_eq!(
            serial_console_token("console=ttyAMA0,115200n8"),
            Some("console=ttyAMA0,115200n8")
        );
        assert!(serial_console_token("console=ttyS0").is_some());
        assert!(serial_console_token("8250.nr_uarts=1").is_none());
    }

    #[test]
    fn config_txt_reads_uart_and_bt_overlay() {
        let parsed = parse_config_txt("# comment\n[pi0w]\nenable_uart=1\ndtoverlay=disable-bt\n");
        assert_eq!(parsed.enable_uart, Some(true));
        assert!(parsed.disable_bt);

        let parsed = parse_config_txt("enable_uart = 0\n");
        assert_eq!(parsed.enable_uart, Some(false));
        assert!(!parsed.disable_bt);

        let parsed = parse_config_txt("dtoverlay=miniuart-bt\n");
        assert!(parsed.disable_bt);

        let parsed = parse_config_txt("dtoverlay=disable-wifi\n");
        assert!(!parsed.disable_bt);
    }

    #[test]
    fn uart_console_fails_when_login_shell_owns_the_port() {
        let check = check_uart_console(&ConsoleProbe {
            cmdline: "console=serial0,115200 console=tty1".into(),
            stdout_path: None,
            model: Some("Raspberry Pi Zero 2 W Rev 1.0".into()),
            config: Some((
                "/boot/firmware/config.txt".into(),
                "enable_uart=1\ndtoverlay=disable-bt\n".into(),
            )),
        });
        assert_eq!(check.status, Status::Fail);
        assert!(check.detail.contains("serial0"), "{}", check.detail);
    }

    #[test]
    fn uart_console_fails_when_zero_w_leaves_bluetooth_on_pl011() {
        let check = check_uart_console(&ConsoleProbe {
            cmdline: "console=tty1".into(),
            stdout_path: None,
            model: Some("Raspberry Pi Zero 2 W Rev 1.0".into()),
            config: Some(("/boot/firmware/config.txt".into(), "enable_uart=1\n".into())),
        });
        assert_eq!(check.status, Status::Fail);
        assert!(check.detail.contains("disable-bt"), "{}", check.detail);
    }

    #[test]
    fn uart_console_passes_a_configured_zero_w() {
        let check = check_uart_console(&ConsoleProbe {
            cmdline: "console=tty1".into(),
            stdout_path: None,
            model: Some("Raspberry Pi Zero 2 W Rev 1.0".into()),
            config: Some((
                "/boot/firmware/config.txt".into(),
                "enable_uart=1\ndtoverlay=disable-bt\n".into(),
            )),
        });
        assert_eq!(check.status, Status::Pass);
    }

    #[test]
    fn uart_console_skips_overlay_when_config_is_absent() {
        let check = check_uart_console(&ConsoleProbe {
            cmdline: "console=tty1".into(),
            stdout_path: None,
            model: None,
            config: None,
        });
        assert_eq!(check.status, Status::Pass);
        assert_eq!(check.detail, "no serial console");
    }

    #[test]
    fn uart_console_ignores_a_pc_hypervisor_serial() {
        let check = check_uart_console(&ConsoleProbe {
            cmdline: "console=ttyS0 8250.nr_uarts=0".into(),
            stdout_path: None,
            model: None,
            config: None,
        });
        assert_eq!(check.status, Status::Pass);
    }

    #[test]
    fn clock_warns_on_epoch() {
        let warn = check_clock(UNIX_EPOCH);
        assert_eq!(warn.status, Status::Warn);
        let pass = check_clock(UNIX_EPOCH + Duration::from_secs(CLOCK_OK_AFTER_S));
        assert_eq!(pass.status, Status::Pass);
    }

    #[test]
    fn csv_rejects_a_directory() {
        let dir = std::env::temp_dir().join(format!("trck-doctor-csv-dir-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let check = check_csv(&dir);
        assert_eq!(check.status, Status::Fail);
        assert!(check.detail.contains("directory"), "{}", check.detail);
        let _ = fs::remove_dir_all(&dir);
    }
}
