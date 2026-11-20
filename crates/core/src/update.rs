//! 检查更新: which release is the newest. The app asks GitHub for
//! `releases/latest`, which answers with a redirect to the newest release's
//! page; the version is read off that address, so nothing about the user is
//! sent. (The JSON API would say the same, but it is rate-limited per address
//! and fails behind a shared one.) With 自动下载更新 on, the release's package
//! for this computer is then fetched and held against its `SHA256SUMS`; what
//! is done with it is the app's (`src/updater.rs`).

use sha2::{Digest, Sha256};

pub const PROJECT_URL: &str = "https://github.com/xuzhougeng/WispStudy";
pub const RELEASES_URL: &str = "https://github.com/xuzhougeng/WispStudy/releases";

/// The address whose redirect names the newest release.
pub fn latest_url() -> String {
    format!("{RELEASES_URL}/latest")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    /// The release's page, where its packages are downloaded.
    pub url: String,
}

fn parts(version: &str) -> Option<Vec<u64>> {
    version.split('.').map(|p| p.parse().ok()).collect()
}

/// `a` is a later version than `b` (dotted numbers; a missing part is 0).
pub fn newer(a: &str, b: &str) -> bool {
    let (Some(mut a), Some(mut b)) = (parts(a), parts(b)) else { return false };
    let n = a.len().max(b.len());
    a.resize(n, 0);
    b.resize(n, 0);
    a > b
}

/// The release a `releases/latest` redirect points to. `None` when it points
/// anywhere else: a project without releases redirects to the release list,
/// and only this project's own release pages are ever offered to open.
pub fn release_from_location(location: &str) -> Option<Release> {
    let tag = location.trim().strip_prefix(RELEASES_URL)?.strip_prefix("/tag/")?;
    let version = tag.strip_prefix('v').unwrap_or(tag);
    parts(version)?;
    Some(Release { version: version.to_string(), url: format!("{RELEASES_URL}/tag/{tag}") })
}

/// The package of a release for this kind of computer, named as the
/// packaging scripts name it. `None` where the app does not install a release
/// itself (Linux: a tar.gz unpacked anywhere).
pub fn asset(version: &str, os: &str, arch: &str) -> Option<String> {
    let label = match (os, arch) {
        ("macos", "aarch64") => "macos-arm64.dmg",
        ("macos", "x86_64") => "macos-x64.dmg",
        ("windows", "x86_64") => "windows-x64.zip",
        _ => return None,
    };
    Some(format!("WispStudy-v{version}-{label}"))
}

/// The folder a release's files are downloaded from.
pub fn files_url(release: &Release) -> String {
    release.url.replacen("/tag/", "/download/", 1)
}

/// What a release's `SHA256SUMS` says one of its files hashes to, in lower case.
pub fn checksum(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.split_once(char::is_whitespace)?;
        (file.trim().trim_start_matches('*') == name && hash.len() == 64).then(|| hash.to_lowercase())
    })
}

pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// The files of the Windows package (a zip of one folder), each under its
/// name without the folder: nothing in an archive says where a file goes.
pub fn unpack(zip: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let broken = || "更新包不完整，无法解开。".to_string();
    let zip = crate::docx::Zip::read(zip).map_err(|_| broken())?;
    let mut files = vec![];
    for entry in zip.names() {
        // Windows PowerShell writes `\` between folder and file, everything else `/`.
        let name = entry.rsplit(['/', '\\']).next().unwrap_or_default();
        if name.is_empty() { continue }
        files.push((name.to_string(), zip.get(entry).ok().flatten().ok_or_else(broken)?));
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(newer("0.2.0", "0.1.9") && newer("0.10.0", "0.9.9") && newer("1.0", "0.9.9") && newer("0.1.0.1", "0.1.0"));
        assert!(!newer("0.1.0", "0.1.0") && !newer("0.1", "0.1.0") && !newer("0.1.0", "0.2.0"));
        // Not a dotted number: never offered.
        assert!(!newer("0.2.0-beta", "0.1.0") && !newer("", "0.1.0"));
    }

    #[test]
    fn redirects() {
        let release = release_from_location("https://github.com/xuzhougeng/WispStudy/releases/tag/v0.2.0").unwrap();
        assert_eq!((release.version.as_str(), release.url.as_str()), ("0.2.0", "https://github.com/xuzhougeng/WispStudy/releases/tag/v0.2.0"));
        assert_eq!(release_from_location("https://github.com/xuzhougeng/WispStudy/releases/tag/1.3").unwrap().version, "1.3");
        // No release yet, another project, a tag that is not a version.
        for location in ["https://github.com/xuzhougeng/WispStudy/releases", "https://github.com/someone/else/releases/tag/v9.0.0", "https://github.com/xuzhougeng/WispStudy/releases/tag/nightly", "https://github.com/xuzhougeng/WispStudy/releases/tag/v1.0/../../x", ""] {
            assert_eq!(release_from_location(location), None, "{location}");
        }
    }

    #[test]
    fn packages() {
        assert_eq!(asset("0.9.1", "macos", "aarch64").as_deref(), Some("WispStudy-v0.9.1-macos-arm64.dmg"));
        assert_eq!(asset("0.9.1", "macos", "x86_64").as_deref(), Some("WispStudy-v0.9.1-macos-x64.dmg"));
        assert_eq!(asset("0.9.1", "windows", "x86_64").as_deref(), Some("WispStudy-v0.9.1-windows-x64.zip"));
        assert_eq!((asset("0.9.1", "linux", "x86_64"), asset("0.9.1", "windows", "aarch64")), (None, None));
        let release = release_from_location("https://github.com/xuzhougeng/WispStudy/releases/tag/v0.9.1").unwrap();
        assert_eq!(files_url(&release), "https://github.com/xuzhougeng/WispStudy/releases/download/v0.9.1");

        // `sha256sum` writes two spaces, or a space and `*` for a binary file.
        let hash = sha256(b"package");
        let sums = format!("{}  other.dmg\n{}  a.dmg\n{} *b.zip\nshort  c.zip\n", "0".repeat(64), hash.to_uppercase(), hash);
        assert_eq!((checksum(&sums, "a.dmg"), checksum(&sums, "b.zip")), (Some(hash.clone()), Some(hash)));
        assert_eq!((checksum(&sums, "c.zip"), checksum(&sums, "missing")), (None, None));
    }

    #[test]
    fn a_package_is_unpacked_without_its_folder() {
        let mut zip = crate::docx::Zip::default();
        zip.set("WispStudy-v0.9.1-windows-x64/", b"");
        zip.set("WispStudy-v0.9.1-windows-x64/WispStudy.exe", &b"app ".repeat(100));
        zip.set("WispStudy-v0.9.1-windows-x64\\wisp.exe", b"cli");
        zip.set("../../outside.txt", b"x");
        let files = unpack(&zip.write()).unwrap();
        assert_eq!(files.iter().map(|(name, data)| (name.as_str(), data.len())).collect::<Vec<_>>(), [("WispStudy.exe", 400), ("wisp.exe", 3), ("outside.txt", 1)]);
        assert!(unpack(b"not a zip").is_err());
    }
}
