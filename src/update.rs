//! Replace this binary with a GitHub release of `igormenato/adsb`.

use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind, Read as _};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Output};

use anyhow::{Context as _, Result, anyhow, bail};
use clap::Parser;
use serde::Deserialize;

const REPO: &str = "igormenato/adsb";
const ASSET_NAME: &str = "trck-aarch64";
const CURRENT: &str = env!("CARGO_PKG_VERSION");
const USER_AGENT: &str = concat!("User-Agent: trck/", env!("CARGO_PKG_VERSION"));

#[derive(Parser, Debug)]
pub struct Args {
    /// Install this tag (`X.Y.Z` or `vX.Y.Z`). Allows downgrades.
    #[arg(long, value_name = "X.Y.Z")]
    version: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl Version {
    fn parse(text: &str) -> Result<Self> {
        let text = text.strip_prefix('v').unwrap_or(text);
        let mut parts = text.split('.');
        let major = parse_part(parts.next(), text)?;
        let minor = parse_part(parts.next(), text)?;
        let patch = parse_part(parts.next(), text)?;
        if parts.next().is_some() {
            bail!("invalid version {text}; expected X.Y.Z");
        }
        Ok(Self {
            major,
            minor,
            patch,
        })
    }

    fn tag(self) -> String {
        format!("v{self}")
    }
}

fn parse_part(part: Option<&str>, text: &str) -> Result<u64> {
    let Some(part) = part else {
        bail!("invalid version {text}; expected X.Y.Z");
    };
    if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
        bail!("invalid version {text}; expected X.Y.Z");
    }
    part.parse()
        .map_err(|_| anyhow!("invalid version {text}; expected X.Y.Z"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Asset {
    name: String,
    size: u64,
    url: String,
    sha256: Option<String>,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    size: u64,
    browser_download_url: String,
    #[serde(default)]
    digest: Option<String>,
}

pub fn run(args: Args) -> ExitCode {
    match run_inner(args) {
        Ok(false) => ExitCode::SUCCESS,
        Ok(true) => {
            eprintln!("restart");
            ExitCode::from(1)
        }
        Err(err) => {
            eprintln!("{err:#}");
            ExitCode::from(1)
        }
    }
}

fn run_inner(args: Args) -> Result<bool> {
    let current = Version::parse(CURRENT).expect("CARGO_PKG_VERSION is X.Y.Z");
    let exact = args.version.as_deref().map(Version::parse).transpose()?;
    if let Some(v) = exact
        && current == v
    {
        println!("trck {current} is up to date");
        return Ok(false);
    }
    let (target, assets) = match exact {
        Some(v) => fetch_tag(&v.tag())?,
        None => fetch_latest()?,
    };
    if exact.is_none() && current >= target {
        println!("trck {current} is up to date");
        return Ok(false);
    }
    let asset = pick_asset(&assets)
        .ok_or_else(|| anyhow!("release {} has no {ASSET_NAME} asset", target.tag()))?;
    let dest = std::env::current_exe().context("cannot resolve this binary")?;
    install(&dest, asset)?;
    println!("{current} -> {target}");
    println!("replaced {}", dest.display());
    Ok(true)
}

fn pick_asset(assets: &[Asset]) -> Option<&Asset> {
    assets.iter().find(|a| a.name == ASSET_NAME)
}

fn install(dest: &Path, asset: &Asset) -> Result<()> {
    let tmp = sibling_new(dest)?;
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)
        .map_err(|err| write_fail(dest, err))?;
    if let Err(err) = curl_file(&asset.url, &tmp) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    if let Err(err) = verify(&tmp, asset) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    make_executable(&tmp).map_err(|err| {
        let _ = fs::remove_file(&tmp);
        write_fail(dest, err)
    })?;
    fs::rename(&tmp, dest).map_err(|err| {
        let _ = fs::remove_file(&tmp);
        write_fail(dest, err)
    })?;
    Ok(())
}

fn sibling_new(dest: &Path) -> Result<PathBuf> {
    let name = dest
        .file_name()
        .ok_or_else(|| anyhow!("binary path {} has no file name", dest.display()))?;
    Ok(dest.with_file_name(format!("{}.new", name.to_string_lossy())))
}

fn verify(path: &Path, asset: &Asset) -> Result<()> {
    let meta = fs::metadata(path).with_context(|| format!("cannot read {}", path.display()))?;
    if meta.len() != asset.size {
        bail!(
            "{ASSET_NAME} is {} bytes, GitHub lists {}",
            meta.len(),
            asset.size
        );
    }
    let mut magic = [0u8; 4];
    fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .with_context(|| format!("cannot read {}", path.display()))?;
    if magic != *b"\x7fELF" {
        bail!("{ASSET_NAME} is not an ELF binary");
    }
    if let Some(expect) = asset.sha256.as_deref() {
        let got = sha256_hex(path)?;
        if got != expect {
            bail!("{ASSET_NAME} sha256 does not match GitHub");
        }
    }
    Ok(())
}

fn sha256_hex(path: &Path) -> Result<String> {
    let out = Command::new("sha256sum")
        .arg(path)
        .output()
        .map_err(|err| {
            if err.kind() == ErrorKind::NotFound {
                anyhow!("{ASSET_NAME} has a GitHub digest, but sha256sum is missing")
            } else {
                anyhow!("cannot run sha256sum: {err}")
            }
        })?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("{}", err.trim());
    }
    let text = String::from_utf8(out.stdout).context("sha256sum output")?;
    let hex = text
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow!("sha256sum output was empty"))?;
    Ok(hex.to_ascii_lowercase())
}

fn make_executable(path: &Path) -> io::Result<()> {
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)
}

fn write_fail(dest: &Path, err: io::Error) -> anyhow::Error {
    if err.kind() == ErrorKind::PermissionDenied {
        anyhow!(
            "cannot replace {}: permission denied (try sudo)",
            dest.display()
        )
    } else {
        anyhow!("cannot replace {}: {err}", dest.display())
    }
}

fn fetch_latest() -> Result<(Version, Vec<Asset>)> {
    fetch_release(&format!(
        "https://api.github.com/repos/{REPO}/releases/latest"
    ))
}

fn fetch_tag(tag: &str) -> Result<(Version, Vec<Asset>)> {
    fetch_release(&format!(
        "https://api.github.com/repos/{REPO}/releases/tags/{tag}"
    ))
}

fn fetch_release(url: &str) -> Result<(Version, Vec<Asset>)> {
    parse_release(&curl_text(url)?)
}

fn parse_release(body: &str) -> Result<(Version, Vec<Asset>)> {
    let raw: GhRelease = serde_json::from_str(body).context("GitHub release JSON")?;
    let version = Version::parse(&raw.tag_name)?;
    let assets = raw
        .assets
        .into_iter()
        .map(|a| {
            Ok(Asset {
                sha256: match a.digest.as_deref() {
                    Some(d) if !d.is_empty() => Some(parse_digest(d)?),
                    _ => None,
                },
                name: a.name,
                size: a.size,
                url: a.browser_download_url,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((version, assets))
}

fn parse_digest(text: &str) -> Result<String> {
    let hex = text
        .strip_prefix("sha256:")
        .ok_or_else(|| anyhow!("unsupported GitHub digest {text}"))?;
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid GitHub sha256 digest");
    }
    Ok(hex.to_ascii_lowercase())
}

fn curl_text(url: &str) -> Result<String> {
    let out = curl(&["-fsSL", "-H", USER_AGENT, url])?;
    String::from_utf8(out.stdout).context("GitHub release JSON")
}

fn curl_file(url: &str, dest: &Path) -> Result<()> {
    let dest = dest
        .to_str()
        .ok_or_else(|| anyhow!("binary path {} is not UTF-8", dest.display()))?;
    curl(&["-fsSL", "-o", dest, url]).map(|_| ())
}

fn curl(args: &[&str]) -> Result<Output> {
    let out = Command::new("curl")
        .args(args)
        .output()
        .map_err(curl_spawn)?;
    if out.status.success() {
        return Ok(out);
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let err = err.trim();
    if err.is_empty() {
        bail!("curl failed");
    }
    bail!("{err}")
}

fn curl_spawn(err: io::Error) -> anyhow::Error {
    if err.kind() == ErrorKind::NotFound {
        anyhow!("trck update needs curl: sudo apt install curl")
    } else {
        anyhow!("cannot run curl: {err}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "trck-update-{}-{}",
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
    fn version_parse_and_order() {
        let a = Version::parse("0.3.3").unwrap();
        let b = Version::parse("v0.3.4").unwrap();
        assert!(a < b);
        assert_eq!(a.to_string(), "0.3.3");
        assert_eq!(b.tag(), "v0.3.4");
        assert_eq!(Version::parse("0.3.4").unwrap(), b);
        assert!(Version::parse("1").is_err());
        assert!(Version::parse("0.3.4.1").is_err());
        assert!(Version::parse("v0.3").is_err());
        assert!(Version::parse("0.3.x").is_err());
    }

    #[test]
    fn pick_aarch64_asset() {
        let assets = [
            Asset {
                name: "source.tar.gz".into(),
                size: 1,
                url: "https://example/src".into(),
                sha256: None,
            },
            Asset {
                name: ASSET_NAME.into(),
                size: 8,
                url: "https://example/trck".into(),
                sha256: None,
            },
        ];
        let got = pick_asset(&assets).unwrap();
        assert_eq!(got.url, "https://example/trck");
        assert!(pick_asset(&assets[..1]).is_none());
    }

    #[test]
    fn verify_size_elf_and_sha256() {
        let dir = temp_dir();
        let path = dir.join("blob");
        let mut elf = b"\x7fELF".to_vec();
        elf.extend_from_slice(&[0u8; 12]);
        fs::write(&path, &elf).unwrap();
        let sha = sha256_hex(&path).unwrap();
        let asset = Asset {
            name: ASSET_NAME.into(),
            size: elf.len() as u64,
            url: String::new(),
            sha256: Some(sha),
        };
        verify(&path, &asset).unwrap();

        let mut bad_hash = asset.clone();
        bad_hash.sha256 = Some("0".repeat(64));
        assert!(verify(&path, &bad_hash).is_err());

        let mut bad_size = asset.clone();
        bad_size.size = 3;
        bad_size.sha256 = None;
        assert!(verify(&path, &bad_size).is_err());

        fs::write(&path, b"PK\x03\x04").unwrap();
        let not_elf = Asset {
            sha256: None,
            size: 4,
            ..asset
        };
        assert!(verify(&path, &not_elf).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_release_reads_digest() {
        let json = r#"{
            "tag_name": "v0.3.4",
            "assets": [{
                "name": "trck-aarch64",
                "size": 16,
                "browser_download_url": "https://example/trck-aarch64",
                "digest": "sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
            }]
        }"#;
        let (ver, assets) = parse_release(json).unwrap();
        assert_eq!(ver.to_string(), "0.3.4");
        let asset = pick_asset(&assets).unwrap();
        assert_eq!(asset.size, 16);
        assert_eq!(
            asset.sha256.as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
    }

    #[test]
    fn atomic_replace_overwrites_temp_file() {
        let dir = temp_dir();
        let dest = dir.join("trck");
        fs::write(&dest, b"old").unwrap();
        let tmp = sibling_new(&dest).unwrap();
        fs::write(&tmp, b"\x7fELFnew").unwrap();
        make_executable(&tmp).unwrap();
        fs::rename(&tmp, &dest).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"\x7fELFnew");
        assert!(!dir.join("trck.new").exists());
        let mode = fs::metadata(&dest).unwrap().permissions().mode() & 0o111;
        assert_ne!(mode, 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn permission_denied_hints_sudo() {
        let err = io::Error::new(ErrorKind::PermissionDenied, "denied");
        let msg = write_fail(Path::new("/usr/local/bin/trck"), err).to_string();
        assert!(msg.contains("permission denied"));
        assert!(msg.contains("sudo"));
    }

    #[test]
    fn missing_curl_names_apt() {
        let msg = curl_spawn(io::Error::new(ErrorKind::NotFound, "nope")).to_string();
        assert_eq!(msg, "trck update needs curl: sudo apt install curl");
    }
}
