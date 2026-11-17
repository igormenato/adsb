//! Replace this binary with a GitHub release of `igormenato/adsb`.

use std::fs;
use std::io::{self, ErrorKind};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context as _, Result, anyhow, bail};
use clap::Parser;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const REPO: &str = "igormenato/adsb";
const ASSET_NAME: &str = "trck-aarch64";
const CURRENT: &str = env!("CARGO_PKG_VERSION");

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
    sha256: Option<[u8; 32]>,
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
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err:#}");
            ExitCode::from(1)
        }
    }
}

fn run_inner(args: Args) -> Result<()> {
    let current = Version::parse(CURRENT).expect("CARGO_PKG_VERSION is X.Y.Z");
    let exact = args.version.as_deref().map(Version::parse).transpose()?;
    if let Some(v) = exact
        && current == v
    {
        println!("trck {current} is up to date");
        return Ok(());
    }
    let (target, assets) = match exact {
        Some(v) => fetch_tag(&v.tag())?,
        None => fetch_latest()?,
    };
    if exact.is_none() && current >= target {
        println!("trck {current} is up to date");
        return Ok(());
    }
    let asset = pick_asset(&assets)
        .ok_or_else(|| anyhow!("release {} has no {ASSET_NAME} asset", target.tag()))?;
    let bytes = http_bytes(&asset.url)?;
    verify(&bytes, asset)?;
    let dest = std::env::current_exe().context("cannot resolve this binary")?;
    replace_atomically(&dest, &bytes)?;
    println!("{current} -> {target}");
    println!("replaced {}", dest.display());
    Ok(())
}

fn pick_asset(assets: &[Asset]) -> Option<&Asset> {
    assets.iter().find(|a| a.name == ASSET_NAME)
}

fn verify(bytes: &[u8], asset: &Asset) -> Result<()> {
    if bytes.len() as u64 != asset.size {
        bail!(
            "{ASSET_NAME} is {} bytes, GitHub lists {}",
            bytes.len(),
            asset.size
        );
    }
    if bytes.len() < 4 || bytes[..4] != *b"\x7fELF" {
        bail!("{ASSET_NAME} is not an ELF binary");
    }
    if let Some(expect) = asset.sha256 {
        let got: [u8; 32] = Sha256::digest(bytes).into();
        if got != expect {
            bail!("{ASSET_NAME} sha256 does not match GitHub");
        }
    }
    Ok(())
}

fn replace_atomically(dest: &Path, bytes: &[u8]) -> Result<()> {
    let name = dest
        .file_name()
        .ok_or_else(|| anyhow!("binary path {} has no file name", dest.display()))?;
    let tmp = dest.with_file_name(format!("{}.new", name.to_string_lossy()));
    write_executable(&tmp, bytes).map_err(|err| write_fail(dest, err))?;
    fs::rename(&tmp, dest).map_err(|err| {
        let _ = fs::remove_file(&tmp);
        write_fail(dest, err)
    })?;
    Ok(())
}

fn write_executable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    fs::write(path, bytes)?;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)?;
    Ok(())
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
    let body = http_text(url)?;
    parse_release(&body)
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

fn parse_digest(text: &str) -> Result<[u8; 32]> {
    let hex = text
        .strip_prefix("sha256:")
        .ok_or_else(|| anyhow!("unsupported GitHub digest {text}"))?;
    let mut out = [0u8; 32];
    hex::decode_to_slice(hex, &mut out).map_err(|_| anyhow!("invalid GitHub sha256 digest"))?;
    Ok(out)
}

fn http_text(url: &str) -> Result<String> {
    let mut resp = get(url)?;
    resp.body_mut()
        .read_to_string()
        .map_err(|err| net_fail(&err))
}

fn http_bytes(url: &str) -> Result<Vec<u8>> {
    let mut resp = get(url)?;
    resp.body_mut().read_to_vec().map_err(|err| net_fail(&err))
}

fn get(url: &str) -> Result<ureq::http::Response<ureq::Body>> {
    ureq::get(url)
        .header("User-Agent", concat!("trck/", env!("CARGO_PKG_VERSION")))
        .call()
        .map_err(http_fail)
}

fn http_fail(err: ureq::Error) -> anyhow::Error {
    match &err {
        ureq::Error::StatusCode(404) => anyhow!("no GitHub release at that tag"),
        ureq::Error::HostNotFound | ureq::Error::ConnectionFailed | ureq::Error::Timeout(_) => {
            anyhow!("cannot reach GitHub: {err}")
        }
        ureq::Error::Io(io)
            if io.kind() == ErrorKind::ConnectionRefused
                || io.kind() == ErrorKind::ConnectionReset
                || io.kind() == ErrorKind::TimedOut
                || io.kind() == ErrorKind::NetworkUnreachable
                || io.kind() == ErrorKind::HostUnreachable =>
        {
            anyhow!("cannot reach GitHub: {err}")
        }
        other => anyhow!("{other}"),
    }
}

fn net_fail(err: &impl std::fmt::Display) -> anyhow::Error {
    anyhow!("cannot reach GitHub: {err}")
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut elf = b"\x7fELF".to_vec();
        elf.extend_from_slice(&[0u8; 12]);
        let sha: [u8; 32] = Sha256::digest(&elf).into();
        let asset = Asset {
            name: ASSET_NAME.into(),
            size: elf.len() as u64,
            url: String::new(),
            sha256: Some(sha),
        };
        verify(&elf, &asset).unwrap();

        let mut bad_hash = asset.clone();
        bad_hash.sha256 = Some([0; 32]);
        assert!(verify(&elf, &bad_hash).is_err());

        let mut bad_size = asset.clone();
        bad_size.size = 3;
        assert!(verify(&elf, &bad_size).is_err());

        let not_elf = Asset {
            sha256: None,
            size: 4,
            ..asset
        };
        assert!(verify(b"PK\x03\x04", &not_elf).is_err());
    }

    #[test]
    fn parse_release_reads_digest() {
        let json = r#"{
            "tag_name": "v0.3.4",
            "assets": [{
                "name": "trck-aarch64",
                "size": 16,
                "browser_download_url": "https://example/trck-aarch64",
                "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            }]
        }"#;
        let (ver, assets) = parse_release(json).unwrap();
        assert_eq!(ver.to_string(), "0.3.4");
        let asset = pick_asset(&assets).unwrap();
        assert_eq!(asset.size, 16);
        assert!(asset.sha256.is_some());
    }

    #[test]
    fn atomic_replace_overwrites_temp_file() {
        let dir = std::env::temp_dir().join(format!("trck-update-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("trck");
        fs::write(&dest, b"old").unwrap();
        replace_atomically(&dest, b"\x7fELFnew").unwrap();
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
}
