//! A whole library in one file, for another Wisp Study: `<name>.wispstudy`.
//!
//! The file is a gzipped tar. Its first entry is `wispstudy.json` (what is
//! inside: the format, the library's name, how many papers and files), so a
//! file can be told apart and described after reading only its head; then
//! the library folder under `library/`: `library.json`, a snapshot of the
//! database taken with `VACUUM INTO` (consistent while the app has it open)
//! and every file beside them but the set-aside ones in `removed/`. Without
//! attachments, `papers/` stays behind and the snapshot loses the records of
//! those files, with what hangs on them (their text, sentences and PDF
//! annotations), so the other side sees papers without full text rather than
//! files that are missing. Untarred
//! by hand, `library/` is a library folder that can be moved into the data
//! folder as it is.
//!
//! Tar is written and read here rather than by a crate: ustar headers, with a
//! PAX record for a path longer than 100 bytes or not ASCII, and for a file
//! over 8 GiB. The reader also takes the GNU long names and skips links,
//! folders and whatever else a hand-made archive may hold.
//!
//! Importing makes a new library with a new id, so the same file can be
//! taken in twice, and a name already in use gets a number.
//!
//! A file from before the app was renamed (Wisp Paper) is `<name>.wisppaper`
//! with `wisppaper.json` first; it is taken in the same way.

use crate::libraries::{self, COLORS, INFO_FILE, LibraryInfo};
use crate::model::new_id;
use crate::store::{DB_FILE, Store};
use crate::time::now_iso;
use anyhow::{Context, Result, anyhow, bail};
use flate2::Compression;
use flate2::read::MultiGzDecoder;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

pub const EXTENSION: &str = "wispstudy";
/// The extension of an archive written before the app was renamed (Wisp Paper); still taken in.
pub const FORMER_EXTENSION: &str = "wisppaper";
/// The first entry, describing the rest.
const MANIFEST: &str = "wispstudy.json";
/// The first entry of an archive written before the rename.
const FORMER_MANIFEST: &str = "wisppaper.json";
/// Where the library folder sits inside the archive.
const FOLDER: &str = "library";
/// Bump when an older build would misread an archive.
pub const FORMAT: u32 = 1;
const NOT_ARCHIVE: &str = "这不是 Wisp Study 文献库文件（.wispstudy），或文件已损坏。";
/// The largest size an ustar header holds: eleven octal digits.
const OCTAL_MAX: u64 = 0o77777777777;
/// The most a manifest, `library.json` or a PAX record may take in memory.
const SMALL: u64 = 1 << 20;

/// What an archive holds, written as its first entry.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Manifest {
    pub format: u32,
    /// The Wisp Study that wrote it.
    pub app_version: String,
    /// `PRAGMA user_version` of the database inside.
    pub schema_version: i32,
    pub name: String,
    pub topic: String,
    /// Papers outside the trash.
    pub papers: usize,
    /// Files beside the database: PDFs, pictures, notes' images.
    pub files: usize,
    /// Bytes of the database and those files, before compression.
    pub bytes: u64,
    /// Whether the full-text PDFs and other attachments came along.
    pub attachments: bool,
    pub exported_at: String,
}

/// Whether `path` is named as a library archive: `.wispstudy`, or `.wisppaper` from before the rename.
pub fn is_archive_name(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case(EXTENSION) || e.eq_ignore_ascii_case(FORMER_EXTENSION))
}

/// A file name for a library's archive: its name, without what a file system refuses.
pub fn file_name(name: &str) -> String {
    format!("{}.{EXTENSION}", file_stem(name, "library"))
}

/// A name as the stem of a file's: without what a file system refuses, and `fallback` when nothing is left.
pub fn file_stem(name: &str, fallback: &str) -> String {
    let clean: String = name.trim().chars().map(|c| if c.is_control() || ['/', '\\', ':', '*', '?', '"', '<', '>', '|'].contains(&c) { '-' } else { c }).collect();
    let clean = clean.trim_matches(['.', ' ']);
    if clean.is_empty() { fallback.to_string() } else { clean.to_string() }
}

// ---- export

/// Pack the library in `info.dir` into `target`, with its full-text PDFs
/// and other attachments or without. Written beside it under another name
/// first, so a failure leaves no half file; an existing file at `target` is
/// replaced only when the new one is whole.
pub fn export(info: &LibraryInfo, target: &Path, attachments: bool) -> Result<Manifest> {
    let database = info.dir.join(DB_FILE);
    if !database.is_file() { bail!("找不到文献库的数据库：{}", database.display()) }
    let snapshot = std::env::temp_dir().join(format!("wisp-export-{}.sqlite", new_id("X")));
    let name = target.file_name().ok_or_else(|| anyhow!("请选择要保存的文件：{}", target.display()))?.to_string_lossy();
    let partial = target.with_file_name(format!("{name}.part"));
    let written = snapshot_database(&database, &snapshot, attachments).and_then(|schema_version| write_archive(info, &snapshot, schema_version, attachments, &partial));
    let _ = std::fs::remove_file(&snapshot);
    let finished = written.and_then(|manifest| {
        std::fs::rename(&partial, target).with_context(|| format!("无法写入 {}", target.display()))?;
        Ok(manifest)
    });
    if finished.is_err() { let _ = std::fs::remove_file(&partial); }
    finished
}

/// A consistent copy of the database, read while another connection may be
/// writing, without the records of the attachments when they stay behind; its schema version.
fn snapshot_database(database: &Path, snapshot: &Path, attachments: bool) -> Result<i32> {
    let db = rusqlite::Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE).with_context(|| format!("无法打开数据库 {}", database.display()))?;
    db.busy_timeout(std::time::Duration::from_secs(10))?;
    let into = snapshot.to_str().ok_or_else(|| anyhow!("临时文件夹的路径无法使用：{}", snapshot.display()))?;
    db.execute("VACUUM INTO ?1", [into]).context("无法复制数据库")?;
    drop(db);
    let copy = rusqlite::Connection::open(snapshot)?;
    if !attachments {
        // What refers to a file goes with it (`ON DELETE CASCADE`); a picture cut from a page keeps its own image.
        copy.pragma_update(None, "foreign_keys", "ON")?;
        copy.execute("DELETE FROM resources", [])?;
        copy.execute_batch("VACUUM")?;
    }
    Ok(copy.pragma_query_value(None, "user_version", |r| r.get(0))?)
}

fn write_archive(info: &LibraryInfo, snapshot: &Path, schema_version: i32, attachments: bool, partial: &Path) -> Result<Manifest> {
    let files = packed_files(&info.dir, attachments)?;
    let snapshot_size = std::fs::metadata(snapshot)?.len();
    let manifest = Manifest {
        format: FORMAT,
        app_version: env!("CARGO_PKG_VERSION").into(),
        schema_version,
        name: info.name.clone(),
        topic: info.topic.clone(),
        papers: Store::count_papers(snapshot),
        files: files.len(),
        bytes: snapshot_size + files.iter().map(|f| f.2).sum::<u64>(),
        attachments,
        exported_at: now_iso(),
    };
    let file = std::fs::File::create(partial).with_context(|| format!("无法写入 {}", partial.display()))?;
    let mut tar = TarWriter { out: GzEncoder::new(BufWriter::new(file), Compression::fast()) };
    tar.add_bytes(MANIFEST, &serde_json::to_vec_pretty(&manifest)?)?;
    tar.add_bytes(&format!("{FOLDER}/{INFO_FILE}"), &serde_json::to_vec_pretty(info)?)?;
    tar.add_file(&format!("{FOLDER}/{DB_FILE}"), snapshot, snapshot_size)?;
    for (relative, path, size) in &files {
        tar.add_file(&format!("{FOLDER}/{relative}"), path, *size)?;
    }
    let mut buffered = tar.finish()?.finish()?;
    buffered.flush()?;
    buffered.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    Ok(manifest)
}

/// The files of a library folder that go with it, as (path inside the
/// folder with `/`, path on disk, size), in order. Left out: the database
/// and its journal (the snapshot stands for them), `library.json` (written
/// from `info`), what was set aside in `removed/`, half-written files and
/// the downloads still being taken in; `papers/` too, without attachments.
fn packed_files(dir: &Path, attachments: bool) -> Result<Vec<(String, PathBuf, u64)>> {
    let mut found = vec![];
    let mut folders = vec![(String::new(), dir.to_path_buf())];
    while let Some((prefix, folder)) = folders.pop() {
        for entry in std::fs::read_dir(&folder).with_context(|| format!("无法读取文件夹 {}", folder.display()))? {
            let entry = entry?;
            // A name that is not Unicode cannot be written into the archive the same on every system.
            let Ok(name) = entry.file_name().into_string() else { continue };
            if name == ".DS_Store" { continue }
            let relative = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            let kind = entry.file_type()?;
            if kind.is_dir() {
                if prefix.is_empty() && (name == "removed" || (name == "papers" && !attachments)) { continue }
                if prefix == "papers" && name.starts_with("fetching-") { continue }
                folders.push((relative, entry.path()));
            } else if kind.is_file() {
                if prefix.is_empty() && (name == INFO_FILE || name.starts_with(DB_FILE)) { continue }
                if name.ends_with(".tmp") || name.ends_with(".part") { continue }
                found.push((relative, entry.path(), entry.metadata()?.len()));
            }
        }
    }
    found.sort();
    Ok(found)
}

// ---- import

/// What the archive at `path` holds, read from its head only.
pub fn peek(path: &Path) -> Result<Manifest> {
    let mut tar = open(path)?;
    read_manifest(&mut tar)
}

/// Take the archive at `path` in as a new library under `root`. It is
/// unpacked into a hidden folder there, its database opened once (which
/// refuses one from a newer Wisp Study and brings an older one up to date)
/// and only then moved to its place, so a failed import leaves nothing on
/// the home page.
pub fn import(root: &Path, path: &Path) -> Result<LibraryInfo> {
    let mut tar = open(path)?;
    read_manifest(&mut tar)?;
    std::fs::create_dir_all(root).with_context(|| format!("无法创建文件夹 {}", root.display()))?;
    let id = new_id("L");
    let staging = root.join(format!(".importing-{id}"));
    let made = unpack(&mut tar, &staging).and_then(|stored| settle(root, &id, &staging, stored));
    if made.is_err() { let _ = std::fs::remove_dir_all(&staging); }
    made
}

fn open(path: &Path) -> Result<TarReader<MultiGzDecoder<BufReader<std::fs::File>>>> {
    let file = std::fs::File::open(path).with_context(|| format!("无法打开 {}", path.display()))?;
    Ok(TarReader { input: MultiGzDecoder::new(BufReader::new(file)), unread: 0, padding: 0 })
}

/// The resource forks a Mac's tar adds before each file (`._name`) when an archive is made by hand.
fn apple_fork(path: &str) -> bool {
    path.rsplit('/').next().is_some_and(|name| name.starts_with("._"))
}

fn read_manifest(tar: &mut TarReader<impl Read>) -> Result<Manifest> {
    let first = loop {
        match tar.next() {
            Ok(Some((path, _))) if apple_fork(&path) => continue,
            Ok(entry) => break entry,
            Err(_) => bail!(NOT_ARCHIVE),
        }
    };
    let Some((path, size)) = first else { bail!(NOT_ARCHIVE) };
    if !matches!(inside(&path).as_deref(), Some(MANIFEST | FORMER_MANIFEST)) || size > SMALL { bail!(NOT_ARCHIVE) }
    let manifest: Manifest = tar.read_small().ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).ok_or_else(|| anyhow!(NOT_ARCHIVE))?;
    if manifest.format == 0 { bail!(NOT_ARCHIVE) }
    if manifest.format > FORMAT { bail!("这个文件由更新版本的 Wisp Study（{}）导出，请升级后再导入。", manifest.app_version) }
    Ok(manifest)
}

/// Every entry under `library/` written into `staging`; the library's own description.
fn unpack(tar: &mut TarReader<impl Read>, staging: &Path) -> Result<LibraryInfo> {
    std::fs::create_dir(staging).with_context(|| format!("无法创建文件夹 {}", staging.display()))?;
    let (mut info, mut database) = (None, false);
    while let Some((path, size)) = tar.next().context(NOT_ARCHIVE)? {
        let Some(relative) = inside(&path).and_then(|p| p.strip_prefix(&format!("{FOLDER}/")).map(str::to_string)) else { continue };
        if apple_fork(&relative) { continue }
        if relative == INFO_FILE {
            if size > SMALL { bail!(NOT_ARCHIVE) }
            info = Some(serde_json::from_slice::<LibraryInfo>(&tar.read_small()?).map_err(|_| anyhow!(NOT_ARCHIVE))?);
            continue;
        }
        database |= relative == DB_FILE;
        let target = staging.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
        if let Some(parent) = target.parent() { std::fs::create_dir_all(parent).with_context(|| format!("无法创建文件夹 {}", parent.display()))? }
        let mut out = BufWriter::new(std::fs::File::create(&target).with_context(|| format!("无法写入 {}", target.display()))?);
        tar.read_into(&mut out).context(NOT_ARCHIVE)?;
        out.flush().with_context(|| format!("无法写入 {}", target.display()))?;
    }
    let info = info.ok_or_else(|| anyhow!("文件中缺少文献库的说明（library.json）。"))?;
    if !database { bail!("文件中缺少文献库的数据库。") }
    Ok(info)
}

/// The unpacked library made one of `root`'s: a new id, a name not in use, opened now.
fn settle(root: &Path, id: &str, staging: &Path, stored: LibraryInfo) -> Result<LibraryInfo> {
    // Closed again before the folder moves: Windows does not move a folder with an open file.
    drop(Store::open(staging)?);
    let now = now_iso();
    let mut info = LibraryInfo {
        id: id.into(),
        name: free_name(root, if stored.name.trim().is_empty() { "导入的文献库" } else { &stored.name }),
        color: if COLORS.contains(&stored.color.as_str()) { stored.color.clone() } else { "blue".into() },
        created_at: if stored.created_at.is_empty() { now.clone() } else { stored.created_at.clone() },
        opened_at: now,
        dir: staging.to_path_buf(),
        ..stored
    };
    info.topic = info.topic.chars().take(2000).collect();
    info.criteria = info.criteria.chars().take(4000).collect();
    libraries::save(&info)?;
    let dir = root.join(id);
    std::fs::rename(staging, &dir).with_context(|| format!("无法移动到 {}", dir.display()))?;
    info.dir = dir;
    info.paper_count = Store::count_papers(&info.dir.join(DB_FILE));
    Ok(info)
}

/// `name`, or with ` (2)`, ` (3)`… when a library under `root` already has it; at most 60 characters.
fn free_name(root: &Path, name: &str) -> String {
    let taken: Vec<String> = libraries::list(root).into_iter().map(|l| l.name).collect();
    let name: String = name.trim().chars().take(60).collect();
    if !taken.contains(&name) { return name }
    (2..)
        .map(|n| {
            let tail = format!(" ({n})");
            let base: String = name.chars().take(60 - tail.chars().count()).collect();
            format!("{}{tail}", base.trim_end())
        })
        .find(|candidate| !taken.contains(candidate))
        .unwrap_or(name)
}

/// A path of the archive as `a/b/c`, when it stays inside wherever it is unpacked:
/// no `..`, no drive, no backslash; `.` and empty parts dropped.
fn inside(path: &str) -> Option<String> {
    let mut parts = vec![];
    for part in path.split('/') {
        match part {
            "" | "." => continue,
            ".." => return None,
            part if part.contains(['\\', ':', '\0']) => return None,
            part => parts.push(part),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

// ---- tar

struct TarWriter<W: Write> {
    out: W,
}

impl<W: Write> TarWriter<W> {
    fn add_bytes(&mut self, name: &str, bytes: &[u8]) -> Result<()> {
        self.header(name, bytes.len() as u64, now_seconds())?;
        self.out.write_all(bytes)?;
        self.pad(bytes.len() as u64)
    }

    /// The `size` bytes of the file at `path`; a file that changed size meanwhile is an error.
    fn add_file(&mut self, name: &str, path: &Path, size: u64) -> Result<()> {
        let file = std::fs::File::open(path).with_context(|| format!("无法读取 {}", path.display()))?;
        let modified = file.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
        self.header(name, size, modified)?;
        let copied = std::io::copy(&mut BufReader::new(file).take(size), &mut self.out).with_context(|| format!("无法读取 {}", path.display()))?;
        if copied < size { bail!("导出时文件被改动了：{}", path.display()) }
        self.pad(size)
    }

    fn header(&mut self, name: &str, size: u64, modified: u64) -> Result<()> {
        let plain = name.is_ascii() && name.len() <= 100;
        if !plain || size > OCTAL_MAX {
            let mut records = String::new();
            if !plain { records.push_str(&pax_record("path", name)) }
            if size > OCTAL_MAX { records.push_str(&pax_record("size", &size.to_string())) }
            self.out.write_all(&header("././@PaxHeader", records.len() as u64, modified, b'x'))?;
            self.out.write_all(records.as_bytes())?;
            self.pad(records.len() as u64)?;
        }
        // What a reader without PAX shows instead of the real name.
        let short: String = name.chars().map(|c| if c.is_ascii() { c } else { '_' }).collect();
        let short = &short[short.len().saturating_sub(100)..];
        self.out.write_all(&header(short, if size > OCTAL_MAX { 0 } else { size }, modified, b'0'))?;
        Ok(())
    }

    fn pad(&mut self, size: u64) -> Result<()> {
        self.out.write_all(&[0; 512][..padding(size) as usize])?;
        Ok(())
    }

    /// The two empty blocks that end an archive; the writer underneath.
    fn finish(mut self) -> Result<W> {
        self.out.write_all(&[0; 1024])?;
        Ok(self.out)
    }
}

fn now_seconds() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn padding(size: u64) -> u64 {
    (512 - size % 512) % 512
}

/// An ustar header block for a regular file (`kind` `0`) or a PAX record (`x`).
fn header(name: &str, size: u64, modified: u64, kind: u8) -> [u8; 512] {
    let mut block = [0u8; 512];
    let name = &name.as_bytes()[..name.len().min(100)];
    block[..name.len()].copy_from_slice(name);
    octal(&mut block[100..108], 0o644);
    octal(&mut block[108..116], 0);
    octal(&mut block[116..124], 0);
    octal(&mut block[124..136], size);
    octal(&mut block[136..148], modified.min(OCTAL_MAX));
    block[156] = kind;
    block[257..263].copy_from_slice(b"ustar\0");
    block[263..265].copy_from_slice(b"00");
    block[148..156].fill(b' ');
    let sum: u64 = block.iter().map(|&b| u64::from(b)).sum();
    // Six digits and a NUL; the eighth byte stays a space.
    octal(&mut block[148..155], sum);
    block
}

/// `value` in octal, zero-padded to fill `field` but its last byte, which is NUL.
fn octal(field: &mut [u8], value: u64) {
    let digits = field.len() - 1;
    let text = format!("{value:0digits$o}");
    field[..digits].copy_from_slice(&text.as_bytes()[text.len() - digits..]);
    field[digits] = 0;
}

/// `<length> <key>=<value>\n`, the length counting itself.
fn pax_record(key: &str, value: &str) -> String {
    let rest = format!(" {key}={value}\n");
    let mut length = rest.len();
    loop {
        let total = rest.len() + length.to_string().len();
        if total == length { return format!("{length}{rest}") }
        length = total;
    }
}

struct TarReader<R: Read> {
    input: R,
    /// Bytes of the current entry not read yet, and the padding after them.
    unread: u64,
    padding: u64,
}

impl<R: Read> TarReader<R> {
    /// The next regular file: its path and size. Its bytes are read with
    /// `read_into` or `read_small` before the next call, or skipped by it.
    fn next(&mut self) -> Result<Option<(String, u64)>> {
        self.skip(self.unread + self.padding)?;
        (self.unread, self.padding) = (0, 0);
        let (mut long_path, mut long_size) = (None, None);
        loop {
            let Some(block) = self.block()? else { return Ok(None) };
            if block.iter().all(|&b| b == 0) { return Ok(None) }
            let mut sum: u64 = block.iter().map(|&b| u64::from(b)).sum();
            sum = sum - block[148..156].iter().map(|&b| u64::from(b)).sum::<u64>() + 8 * u64::from(b' ');
            if number(&block[148..156])? != sum { bail!("文件头校验失败") }
            let size = number(&block[124..136])?;
            match block[156] {
                b'x' => {
                    for (key, value) in pax_records(&self.take_small(size)?)? {
                        match key.as_str() {
                            "path" => long_path = Some(value),
                            "size" => long_size = Some(value.parse::<u64>().map_err(|_| anyhow!("文件大小无法读取"))?),
                            _ => {}
                        }
                    }
                }
                b'L' => long_path = Some(String::from_utf8_lossy(&self.take_small(size)?).trim_end_matches('\0').to_string()),
                b'0' | b'\0' | b'7' => {
                    let path = long_path.take().unwrap_or_else(|| ustar_path(&block));
                    let size = long_size.take().unwrap_or(size);
                    (self.unread, self.padding) = (size, padding(size));
                    return Ok(Some((path, size)));
                }
                // Folders, links, global PAX records: nothing to keep.
                _ => {
                    self.skip(size + padding(size))?;
                    (long_path, long_size) = (None, None);
                }
            }
        }
    }

    fn read_into(&mut self, out: &mut dyn Write) -> Result<()> {
        let size = self.unread;
        let copied = std::io::copy(&mut (&mut self.input).take(size), out)?;
        if copied < size { bail!("文件不完整") }
        self.unread = 0;
        Ok(())
    }

    /// The current entry's bytes, when it is small.
    fn read_small(&mut self) -> Result<Vec<u8>> {
        if self.unread > SMALL { bail!("条目过大") }
        let mut bytes = vec![];
        self.read_into(&mut bytes)?;
        Ok(bytes)
    }

    /// A header entry's body (`size` bytes and their padding).
    fn take_small(&mut self, size: u64) -> Result<Vec<u8>> {
        if size > SMALL { bail!("条目过大") }
        let mut bytes = vec![];
        if (&mut self.input).take(size).read_to_end(&mut bytes)? as u64 != size { bail!("文件不完整") }
        self.skip(padding(size))?;
        Ok(bytes)
    }

    fn skip(&mut self, count: u64) -> Result<()> {
        if std::io::copy(&mut (&mut self.input).take(count), &mut std::io::sink())? < count { bail!("文件不完整") }
        Ok(())
    }

    /// The next 512 bytes; `None` at the end of the input.
    fn block(&mut self) -> Result<Option<[u8; 512]>> {
        let mut block = [0u8; 512];
        let mut got = 0;
        while got < 512 {
            match self.input.read(&mut block[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
        match got {
            0 => Ok(None),
            512 => Ok(Some(block)),
            _ => bail!("文件不完整"),
        }
    }
}

/// A number field: octal digits padded with NULs or spaces, or base-256 when its first bit is set.
fn number(field: &[u8]) -> Result<u64> {
    if field[0] & 0x80 != 0 {
        return Ok(field[1..].iter().fold(u64::from(field[0] & 0x7f), |n, &b| n << 8 | u64::from(b)));
    }
    let text = String::from_utf8_lossy(field);
    let digits = text.trim_matches(|c: char| c == '\0' || c == ' ');
    let digits = digits.split(['\0', ' ']).next().unwrap_or("");
    if digits.is_empty() { return Ok(0) }
    u64::from_str_radix(digits, 8).map_err(|_| anyhow!("文件头无法读取"))
}

/// The name of an ustar header, with its prefix.
fn ustar_path(block: &[u8; 512]) -> String {
    let field = |range: std::ops::Range<usize>| {
        let bytes = &block[range];
        String::from_utf8_lossy(&bytes[..bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len())]).into_owned()
    };
    let (name, prefix) = (field(0..100), if &block[257..262] == b"ustar" { field(345..500) } else { String::new() });
    if prefix.is_empty() { name } else { format!("{prefix}/{name}") }
}

fn pax_records(body: &[u8]) -> Result<Vec<(String, String)>> {
    let mut records = vec![];
    let mut at = 0;
    while at < body.len() {
        let space = body[at..].iter().position(|&b| b == b' ').ok_or_else(|| anyhow!("PAX 记录无法读取"))?;
        let length: usize = std::str::from_utf8(&body[at..at + space]).ok().and_then(|n| n.parse().ok()).ok_or_else(|| anyhow!("PAX 记录无法读取"))?;
        if length <= space + 1 || at + length > body.len() { bail!("PAX 记录无法读取") }
        let record = String::from_utf8_lossy(&body[at + space + 1..at + length]);
        let record = record.strip_suffix('\n').unwrap_or(&record);
        if let Some((key, value)) = record.split_once('=') { records.push((key.to_string(), value.to_string())) }
        at += length;
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Paper;

    fn folder(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wisp-archive-{label}-{}", new_id("T")));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_library_goes_to_another_wisp_study_whole() {
        let (here, there, out) = (folder("here"), folder("there"), folder("out"));
        let mut info = libraries::create(&here, "植物根单细胞", "根尖发育", "green").unwrap();
        info.criteria = "只要单细胞".into();
        libraries::save(&info).unwrap();
        // A name past the 100 bytes of an ustar header, and not ASCII.
        let source = out.join("一篇很长的名字".repeat(8) + ".pdf");
        std::fs::write(&source, b"%PDF-1.4 root").unwrap();
        let (whole, light) = (out.join(file_name(&info.name)), out.join("light.wispstudy"));
        assert!(whole.ends_with("植物根单细胞.wispstudy"));
        {
            // The app keeps its database open while it exports.
            let mut store = Store::open(&info.dir).unwrap();
            let paper = store.add_paper(Paper { title: "Root atlas".into(), ..Default::default() }).unwrap();
            let gone = store.add_paper(Paper { title: "Gone".into(), ..Default::default() }).unwrap();
            store.trash_paper(&gone.id).unwrap();
            store.import_file(&paper.id, &source, "fulltext").unwrap();
            let long = info.dir.join("figures").join(format!("{}.png", "图".repeat(40)));
            std::fs::create_dir_all(long.parent().unwrap()).unwrap();
            std::fs::write(&long, b"png").unwrap();
            std::fs::create_dir_all(info.dir.join("removed")).unwrap();
            std::fs::write(info.dir.join("removed").join("old.pdf"), b"old").unwrap();
            let manifest = export(&info, &whole, true).unwrap();
            assert_eq!((manifest.format, manifest.papers, manifest.files, manifest.attachments, manifest.name.as_str()), (FORMAT, 1, 2, true, "植物根单细胞"));
            assert!(!out.join("植物根单细胞.wispstudy.part").exists());
            assert_eq!(peek(&whole).unwrap(), manifest);
            let manifest = export(&info, &light, false).unwrap();
            assert_eq!((manifest.papers, manifest.files, manifest.attachments), (1, 1, false));
            // The library itself keeps its files.
            assert_eq!(store.paper_files(&paper.id).unwrap().len(), 1);
        }

        let first = import(&there, &whole).unwrap();
        let second = import(&there, &whole).unwrap();
        assert_ne!(first.id, info.id);
        assert_eq!((first.name.as_str(), second.name.as_str()), ("植物根单细胞", "植物根单细胞 (2)"));
        assert_eq!((first.topic.as_str(), first.criteria.as_str(), first.color.as_str(), first.paper_count), ("根尖发育", "只要单细胞", "green", 1));
        // Nothing left half-way.
        assert!(std::fs::read_dir(&there).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().starts_with(".importing")));
        let store = Store::open(&first.dir).unwrap();
        let papers = store.list_papers().unwrap();
        assert_eq!((papers.len(), papers[0].title.as_str(), papers[0].fulltext.as_str()), (1, "Root atlas", "stored"));
        let files = store.paper_files(&papers[0].id).unwrap();
        assert_eq!(std::fs::read(store.file_path(&files[0])).unwrap(), b"%PDF-1.4 root");
        assert_eq!(std::fs::read(first.dir.join("figures").join(format!("{}.png", "图".repeat(40)))).unwrap(), b"png");
        assert!(!first.dir.join("removed").exists());

        // Without the PDFs: the papers come, with no file they would miss.
        let third = import(&there, &light).unwrap();
        let store = Store::open(&third.dir).unwrap();
        let papers = store.list_papers().unwrap();
        assert_eq!((papers.len(), papers[0].fulltext.as_str()), (1, "missing"));
        assert!(store.list_files().unwrap().is_empty());
        assert_eq!(std::fs::read_dir(third.dir.join("papers")).unwrap().count(), 0);
        assert_eq!(libraries::list(&there).len(), 3);
        for dir in [here, there, out] { std::fs::remove_dir_all(dir).ok(); }
    }

    #[test]
    fn an_archive_from_before_the_rename_is_taken_in() {
        let (here, there, out) = (folder("former-here"), folder("former-there"), folder("former-out"));
        let info = libraries::create(&here, "旧名", "", "blue").unwrap();
        Store::open(&info.dir).unwrap().add_paper(Paper { title: "Root atlas".into(), ..Default::default() }).unwrap();
        let current = out.join(file_name(&info.name));
        export(&info, &current, false).unwrap();
        // The same archive as a Wisp Paper build wrote it: `.wisppaper`, `wisppaper.json` first.
        let former = out.join("旧名.wisppaper");
        let mut tar = TarWriter { out: GzEncoder::new(std::fs::File::create(&former).unwrap(), Compression::fast()) };
        let mut reader = open(&current).unwrap();
        while let Some((path, _)) = reader.next().unwrap() {
            let mut bytes = vec![];
            reader.read_into(&mut bytes).unwrap();
            tar.add_bytes(if path == MANIFEST { FORMER_MANIFEST } else { path.as_str() }, &bytes).unwrap();
        }
        tar.finish().unwrap().finish().unwrap();
        assert!(is_archive_name(&former) && is_archive_name(&current) && !is_archive_name(&out.join("旧名.zip")));
        assert_eq!(peek(&former).unwrap().name, "旧名");
        let taken = import(&there, &former).unwrap();
        assert_eq!((taken.name.as_str(), taken.paper_count), ("旧名", 1));
        assert_eq!(Store::open(&taken.dir).unwrap().list_papers().unwrap()[0].title, "Root atlas");
        for dir in [here, there, out] { std::fs::remove_dir_all(dir).ok(); }
    }

    #[test]
    fn what_is_not_an_archive_is_refused() {
        let (root, out) = (folder("root"), folder("bad"));
        let text = out.join("a.wispstudy");
        std::fs::write(&text, "not a library").unwrap();
        assert!(import(&root, &text).unwrap_err().to_string().contains("不是 Wisp Study"));
        // A gzipped tar of something else.
        let other = out.join("b.wispstudy");
        let mut tar = TarWriter { out: GzEncoder::new(std::fs::File::create(&other).unwrap(), Compression::fast()) };
        tar.add_bytes("readme.txt", b"hi").unwrap();
        tar.finish().unwrap().finish().unwrap();
        assert!(peek(&other).unwrap_err().to_string().contains("不是 Wisp Study"));
        // From a newer Wisp Study.
        let newer = out.join("c.wispstudy");
        let mut tar = TarWriter { out: GzEncoder::new(std::fs::File::create(&newer).unwrap(), Compression::fast()) };
        tar.add_bytes(MANIFEST, &serde_json::to_vec(&Manifest { format: FORMAT + 1, app_version: "99.0.0".into(), ..Default::default() }).unwrap()).unwrap();
        tar.finish().unwrap().finish().unwrap();
        assert!(import(&root, &newer).unwrap_err().to_string().contains("99.0.0"));
        // Made by hand with a Mac's tar: a fork before the manifest is passed over.
        let forked = out.join("e.wispstudy");
        let mut tar = TarWriter { out: GzEncoder::new(std::fs::File::create(&forked).unwrap(), Compression::fast()) };
        tar.add_bytes("./._wispstudy.json", b"\0\x05\x16\x07").unwrap();
        tar.add_bytes("./wispstudy.json", &serde_json::to_vec(&Manifest { format: FORMAT, name: "手工".into(), ..Default::default() }).unwrap()).unwrap();
        tar.finish().unwrap().finish().unwrap();
        assert_eq!(peek(&forked).unwrap().name, "手工");
        // A path that would climb out of the folder is not written; without a database there is no library.
        let climbing = out.join("d.wispstudy");
        let mut tar = TarWriter { out: GzEncoder::new(std::fs::File::create(&climbing).unwrap(), Compression::fast()) };
        tar.add_bytes(MANIFEST, &serde_json::to_vec(&Manifest { format: FORMAT, ..Default::default() }).unwrap()).unwrap();
        tar.add_bytes("library/library.json", &serde_json::to_vec(&LibraryInfo { name: "x".into(), ..Default::default() }).unwrap()).unwrap();
        tar.add_bytes("library/../../escaped.txt", b"no").unwrap();
        tar.finish().unwrap().finish().unwrap();
        assert!(import(&root, &climbing).unwrap_err().to_string().contains("数据库"));
        assert!(!root.parent().unwrap().join("escaped.txt").exists() && !root.join("escaped.txt").exists());
        assert!(libraries::list(&root).is_empty() && std::fs::read_dir(&root).unwrap().count() == 0);
        for dir in [root, out] { std::fs::remove_dir_all(dir).ok(); }
    }

    #[test]
    fn tar_headers_hold_long_and_large_entries() {
        assert_eq!(pax_record("path", "a"), "9 path=a\n");
        let long = pax_record("path", &"x".repeat(95));
        assert_eq!(long.len().to_string(), long.split(' ').next().unwrap());
        let block = header("library/a.pdf", 1234, 0, b'0');
        assert_eq!((number(&block[124..136]).unwrap(), ustar_path(&block).as_str()), (1234, "library/a.pdf"));
        let mut big = [0u8; 12];
        big[0] = 0x80;
        big[11] = 1;
        assert_eq!(number(&big).unwrap(), 1);
        assert_eq!(number(b"0000644 \0").unwrap(), 0o644);
        let mut bytes = vec![];
        let mut tar = TarWriter { out: &mut bytes };
        tar.header("library/papers/论文.pdf", OCTAL_MAX + 1, 0).unwrap();
        let mut reader = TarReader { input: bytes.as_slice(), unread: 0, padding: 0 };
        // The entry's bytes are not there, but its header says what they are.
        assert_eq!((reader.input.len() % 512, reader.next().ok().flatten()), (0, Some(("library/papers/论文.pdf".to_string(), OCTAL_MAX + 1))));
        assert_eq!(inside("./library/a/../b"), None);
        assert_eq!(inside("/library//papers/./a.pdf").as_deref(), Some("library/papers/a.pdf"));
        assert_eq!(inside("library/C:/x"), None);
        assert_eq!(file_name(" a/b: c? "), "a-b- c-.wispstudy");
        assert_eq!(file_name("..."), "library.wispstudy");
    }
}
