//! 압축 파일의 형식을 알아내고, 내용 목록을 읽고, 폴더에 풉니다.
//!
//! zip, 7z, rar, tar 와 gz/bz2/xz/zst 로 압축한 tar 또는 단일 파일을 다룹니다.
//! 먼저 임시 폴더에 풀고 끝까지 성공했을 때만 제 이름으로 옮기므로, 실패하거나
//! 취소하면 아무것도 남지 않습니다.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, BufReader, Cursor, Read, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context as _};
use zip::result::ZipError;

use crate::archive::{decode_name, open_zip, password_candidates};

/// 열기 대화 상자에 보여 줄 압축 파일 확장자. 실제 판별은 파일 내용으로 합니다.
pub const EXTENSIONS: &[&str] = &[
    "zip", "7z", "rar", "tar", "gz", "tgz", "bz2", "tbz", "tbz2", "xz", "txz", "zst", "tzst",
];

/// 압축 파일 이름에서 떼어 낼 확장자. 긴 것이 먼저 와야 합니다.
const NAME_SUFFIXES: &[&str] = &[
    ".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".zip", ".7z", ".rar", ".tar", ".tgz", ".tbz2",
    ".tbz", ".txz", ".tzst", ".gz", ".bz2", ".xz", ".zst",
];

const COPY_BUFFER_BYTES: usize = 256 * 1024;
const TAR_BLOCK: usize = 512;
/// 백신이나 탐색기가 방금 쓴 파일을 잠깐 잡고 있을 때 이름 바꾸기를 다시 시도하는 횟수.
const RENAME_ATTEMPTS: u32 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Zip,
    SevenZ,
    Rar,
    Tar,
    Gzip,
    Bzip2,
    Xz,
    Zstd,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Format::Zip => "ZIP",
            Format::SevenZ => "7Z",
            Format::Rar => "RAR",
            Format::Tar => "TAR",
            Format::Gzip => "GZ",
            Format::Bzip2 => "BZ2",
            Format::Xz => "XZ",
            Format::Zstd => "ZST",
        }
    }
}

#[derive(Debug)]
pub enum ExtractError {
    /// 암호가 걸려 있는데 암호를 주지 않았습니다.
    PasswordRequired,
    WrongPassword,
    Cancelled,
    Failed(anyhow::Error),
}

impl From<anyhow::Error> for ExtractError {
    fn from(error: anyhow::Error) -> Self {
        ExtractError::Failed(error)
    }
}

impl From<io::Error> for ExtractError {
    fn from(error: io::Error) -> Self {
        ExtractError::Failed(error.into())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct ListedEntry {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
}

#[derive(Debug, Default)]
pub struct Listing {
    pub entries: Vec<ListedEntry>,
    pub files: usize,
    pub total_bytes: u64,
    /// 풀려면 암호가 필요한 항목이 있습니다.
    pub encrypted: bool,
}

impl Listing {
    /// 풀었을 때 생길 경로로 이름을 다듬어 넣습니다. `./`처럼 남는 것이 없는 항목은 뺍니다.
    fn push(&mut self, name: String, size: u64, is_dir: bool) {
        let Some(relative) = safe_relative(&name) else {
            return;
        };
        let mut name = relative.to_string_lossy().replace('\\', "/");
        if is_dir {
            name.push('/');
        }
        if !is_dir {
            self.files += 1;
            self.total_bytes = self.total_bytes.saturating_add(size);
        }
        self.entries.push(ListedEntry { name, size, is_dir });
    }
}

/// 작업 스레드가 쓰고 화면이 읽는 진행 상태.
#[derive(Default)]
pub struct Progress {
    done: AtomicU64,
    total: AtomicU64,
    cancelled: AtomicBool,
    current: Mutex<String>,
}

impl Progress {
    /// 0~1 사이의 진행률. 전체 크기를 아직 모르면 `None`.
    pub fn fraction(&self) -> Option<f32> {
        let total = self.total.load(Ordering::Relaxed);
        let done = self.done.load(Ordering::Relaxed);
        (total > 0).then(|| (done as f64 / total as f64).min(1.0) as f32)
    }

    /// 지금 풀고 있는 항목의 이름.
    pub fn current(&self) -> String {
        self.current.lock().map(|name| name.clone()).unwrap_or_default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    fn add(&self, bytes: u64) {
        self.done.fetch_add(bytes, Ordering::Relaxed);
    }

    fn set_total(&self, bytes: u64) {
        self.total.store(bytes, Ordering::Relaxed);
    }

    fn set_current(&self, name: &str) {
        if let Ok(mut current) = self.current.lock() {
            name.clone_into(&mut current);
        }
    }
}

#[derive(Debug)]
pub struct Extracted {
    /// 풀어서 생긴 폴더, 또는 하나뿐인 파일.
    pub target: PathBuf,
    pub files: u64,
    /// 만들지 않고 건너뛴 링크와 장치 파일의 수.
    pub skipped: u64,
}

/// 백그라운드에서 도는 압축 풀기 한 건.
pub struct ExtractJob {
    pub archive: PathBuf,
    pub format: Format,
    /// 결과가 놓일 폴더.
    pub parent: PathBuf,
    pub progress: Arc<Progress>,
    rx: Receiver<Result<Extracted, ExtractError>>,
    worker: Option<JoinHandle<()>>,
}

impl ExtractJob {
    pub fn spawn(
        ctx: egui::Context,
        archive: PathBuf,
        format: Format,
        parent: PathBuf,
        password: Option<String>,
    ) -> Self {
        let progress = Arc::new(Progress::default());
        let (tx, rx) = mpsc::channel();
        let (source, target, shared) = (archive.clone(), parent.clone(), progress.clone());
        let worker = thread::Builder::new()
            .name("extract".into())
            .spawn(move || {
                let result = extract_into(&source, format, &target, password.as_deref(), &shared);
                let _ = tx.send(result);
                ctx.request_repaint();
            })
            .expect("failed to spawn extract thread");
        Self { archive, format, parent, progress, rx, worker: Some(worker) }
    }

    /// 끝났으면 결과를 돌려줍니다.
    pub fn poll(&mut self) -> Option<Result<Extracted, ExtractError>> {
        match self.rx.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(ExtractError::Failed(anyhow!(
                "압축 풀기 작업이 예기치 않게 끝났습니다"
            )))),
        }
    }
}

impl Drop for ExtractJob {
    /// 풀던 중에 창을 닫아도 임시 폴더가 남지 않도록, 취소시키고 정리가 끝나기를 기다립니다.
    fn drop(&mut self) {
        self.progress.cancel();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

// ----- 형식 판별 -----------------------------------------------------------------

/// 확장자가 아니라 파일 앞부분의 서명으로 압축 형식을 판별합니다.
pub fn detect(path: &Path) -> Option<Format> {
    let mut head = [0u8; TAR_BLOCK];
    let mut file = File::open(path).ok()?;
    let len = read_up_to(&mut file, &mut head).ok()?;
    detect_bytes(&head[..len])
}

fn detect_bytes(head: &[u8]) -> Option<Format> {
    if head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06") {
        Some(Format::Zip)
    } else if head.starts_with(b"7z\xBC\xAF\x27\x1C") {
        Some(Format::SevenZ)
    } else if head.starts_with(b"Rar!\x1A\x07") {
        Some(Format::Rar)
    } else if head.starts_with(&[0x1F, 0x8B, 0x08]) {
        Some(Format::Gzip)
    } else if head.starts_with(b"BZh") && matches!(head.get(3), Some(b'1'..=b'9')) {
        Some(Format::Bzip2)
    } else if head.starts_with(b"\xFD7zXZ\x00") {
        Some(Format::Xz)
    } else if head.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        Some(Format::Zstd)
    } else if is_tar_header(head) {
        Some(Format::Tar)
    } else {
        None
    }
}

/// tar 는 맨 앞에 서명이 없으므로 첫 헤더 블록의 표식이나 체크섬으로 알아봅니다.
fn is_tar_header(head: &[u8]) -> bool {
    let Some(block) = head.get(..TAR_BLOCK) else {
        return false;
    };
    if &block[257..262] == b"ustar" {
        return true;
    }
    // 옛 tar: 체크섬 칸(148..156)을 공백으로 보고 더한 값이 그 칸에 8진수로 적혀 있습니다.
    let field = String::from_utf8_lossy(&block[148..156]);
    let Ok(stored) = u32::from_str_radix(field.trim_matches(|c| c == ' ' || c == '\0'), 8) else {
        return false;
    };
    let sum: u32 = block
        .iter()
        .enumerate()
        .map(|(i, byte)| if (148..156).contains(&i) { 32 } else { u32::from(*byte) })
        .sum();
    // 모두 0인 블록은 합이 256(공백 8개)이라 우연히 맞지 않도록 이름이 있는지도 봅니다.
    sum == stored && block[0] != 0
}

fn read_up_to(reader: &mut dyn Read, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

// ----- 목록 ---------------------------------------------------------------------

/// 압축 파일 안의 항목을 나열합니다. gz 처럼 전부 풀어 봐야 알 수 있는 형식은 `None`.
pub fn list(path: &Path, format: Format, password: Option<&str>) -> Result<Option<Listing>, ExtractError> {
    // unrar 바인딩은 손상된 파일에서 panic 할 수 있습니다. 화면 스레드를 죽이지 않게 막습니다.
    panic::catch_unwind(AssertUnwindSafe(|| match format {
        Format::Zip => list_zip(path).map(Some),
        Format::SevenZ => list_7z(path, password).map(Some),
        Format::Rar => list_rar(path, password).map(Some),
        Format::Tar => list_tar(path).map(Some),
        Format::Gzip | Format::Bzip2 | Format::Xz | Format::Zstd => Ok(None),
    }))
    .unwrap_or_else(|_| Err(broken_archive()))
}

fn broken_archive() -> ExtractError {
    ExtractError::Failed(anyhow!("압축 파일이 손상되어 읽을 수 없습니다"))
}

fn list_zip(path: &Path) -> Result<Listing, ExtractError> {
    let zip = open_zip(path)?;
    let mut listing = Listing::default();
    for index in 0..zip.len() {
        let entry = zip.by_index_data(index).context("zip 항목을 읽지 못했습니다")?;
        listing.encrypted |= entry.encrypted();
        listing.push(decode_name(entry.name_raw()), entry.size(), entry.is_dir());
    }
    Ok(listing)
}

fn list_7z(path: &Path, password: Option<&str>) -> Result<Listing, ExtractError> {
    let reader = open_7z(path, password)?;
    let archive = reader.archive();
    let mut listing = Listing::default();
    for (index, entry) in archive.files.iter().enumerate() {
        listing.encrypted |= archive.is_file_encrypted(index);
        listing.push(entry.name().replace('\\', "/"), entry.size(), entry.is_directory());
    }
    Ok(listing)
}

fn list_rar(path: &Path, password: Option<&str>) -> Result<Listing, ExtractError> {
    let path = rar_first_volume(path);
    let password = password.map(rar_password);
    let archive = rar_archive(&path, password.as_deref())
        .open_for_listing()
        .map_err(|error| rar_error(error, false, password.is_some()))?;
    let encrypted_headers = archive.has_encrypted_headers();
    let mut listing = Listing { encrypted: encrypted_headers, ..Listing::default() };
    for header in archive {
        let header = header.map_err(|error| rar_error(error, encrypted_headers, password.is_some()))?;
        listing.encrypted |= header.is_encrypted();
        let name = header.filename.to_string_lossy().replace('\\', "/");
        listing.push(name, header.unpacked_size, header.is_directory());
    }
    Ok(listing)
}

fn list_tar(path: &Path) -> Result<Listing, ExtractError> {
    let file = File::open(path).with_context(|| format!("cannot open file: {}", path.display()))?;
    let mut archive = tar::Archive::new(BufReader::new(file));
    let mut listing = Listing::default();
    for entry in archive.entries_with_seek().context("tar 파일을 읽지 못했습니다")? {
        let entry = entry.context("tar 파일을 읽지 못했습니다")?;
        let kind = entry.header().entry_type();
        let name = decode_name(&entry.path_bytes());
        let is_dir = is_tar_dir(kind, &name);
        if is_dir || is_tar_file(kind) {
            listing.push(name, entry.size(), is_dir);
        }
    }
    Ok(listing)
}

// ----- 풀기 ---------------------------------------------------------------------

/// `archive`를 `parent` 아래에 풉니다.
///
/// 최상위 항목이 하나뿐이면 그 항목을 그대로 `parent`에 놓고, 여럿이면 압축 파일
/// 이름의 폴더를 만들어 담습니다. 같은 이름이 있으면 `이름 (2)`처럼 번호를 붙입니다.
pub fn extract_into(
    archive: &Path,
    format: Format,
    parent: &Path,
    password: Option<&str>,
    progress: &Progress,
) -> Result<Extracted, ExtractError> {
    // 분할 RAR 의 중간 파일을 열었어도 첫 파일부터 풀리므로, 결과 이름도 첫 파일을 따릅니다.
    let first_volume = (format == Format::Rar).then(|| rar_first_volume(archive));
    let archive = first_volume.as_deref().unwrap_or(archive);
    let name = archive.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let stem = safe_component(strip_archive_suffix(&name));
    let staging = create_staging(parent, &stem)?;

    let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
        let mut sink = Sink::new(&staging, progress);
        extract_entries(archive, format, password, &mut sink)?;
        Ok((sink.files, sink.skipped))
    }))
    .unwrap_or_else(|_| Err(broken_archive()))
    .and_then(|(files, skipped)| {
        Ok(Extracted { target: settle(&staging, parent, &stem)?, files, skipped })
    });

    if outcome.is_err() {
        if let Err(error) = fs::remove_dir_all(&staging) {
            log::warn!("cannot remove {}: {error}", staging.display());
        }
    }
    outcome
}

fn extract_entries(
    archive: &Path,
    format: Format,
    password: Option<&str>,
    sink: &mut Sink<'_>,
) -> Result<(), ExtractError> {
    match format {
        Format::Zip => extract_zip(archive, password, sink),
        Format::SevenZ => extract_7z(archive, password, sink),
        Format::Rar => extract_rar(archive, password, sink),
        Format::Tar | Format::Gzip | Format::Bzip2 | Format::Xz | Format::Zstd => {
            extract_stream(archive, format, sink)
        }
    }
}

fn create_staging(parent: &Path, stem: &str) -> Result<PathBuf, ExtractError> {
    fs::create_dir_all(parent)
        .with_context(|| format!("폴더를 만들지 못했습니다: {}", parent.display()))?;
    for number in 1u32.. {
        let name = match number {
            1 => format!("{stem}.푸는 중"),
            _ => format!("{stem}.푸는 중 ({number})"),
        };
        let staging = parent.join(name);
        match fs::create_dir(&staging) {
            Ok(()) => return Ok(staging),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(anyhow!(error)
                    .context(format!("폴더를 만들지 못했습니다: {}", parent.display()))
                    .into())
            }
        }
    }
    unreachable!("ran out of staging folder names")
}

/// 임시 폴더의 내용을 제 이름으로 옮기고, 결과 경로를 돌려줍니다.
fn settle(staging: &Path, parent: &Path, stem: &str) -> Result<PathBuf, ExtractError> {
    let items = fs::read_dir(staging)
        .and_then(|entries| entries.collect::<io::Result<Vec<_>>>())
        .context("푼 내용을 확인하지 못했습니다")?;
    match items.as_slice() {
        [] => Err(anyhow!("압축 파일 안에 풀 내용이 없습니다").into()),
        [only] => {
            let is_dir = only.file_type().is_ok_and(|kind| kind.is_dir());
            let target = unique_path(parent, &only.file_name(), is_dir);
            rename(&only.path(), &target)?;
            fs::remove_dir(staging).context("임시 폴더를 지우지 못했습니다")?;
            Ok(target)
        }
        _ => {
            let target = unique_path(parent, OsStr::new(stem), true);
            rename(staging, &target)?;
            Ok(target)
        }
    }
}

fn rename(from: &Path, to: &Path) -> Result<(), ExtractError> {
    let mut attempt = 1;
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(error) if attempt < RENAME_ATTEMPTS && error.kind() == io::ErrorKind::PermissionDenied => {
                attempt += 1;
                thread::sleep(Duration::from_millis(150));
            }
            Err(error) => {
                return Err(anyhow!(error)
                    .context(format!("푼 내용을 옮기지 못했습니다: {}", to.display()))
                    .into())
            }
        }
    }
}

/// `parent` 안에서 아직 쓰이지 않은 이름을 고릅니다: `이름`, `이름 (2)`, `이름 (3)` …
fn unique_path(parent: &Path, name: &OsStr, is_dir: bool) -> PathBuf {
    let first = parent.join(name);
    if !first.exists() {
        return first;
    }
    let name = Path::new(name);
    // 폴더 이름의 마침표는 확장자가 아닙니다.
    let (stem, extension) = match (is_dir, name.file_stem(), name.extension()) {
        (false, Some(stem), Some(extension)) => (stem, Some(extension)),
        _ => (name.as_os_str(), None),
    };
    for number in 2u32.. {
        let mut candidate = stem.to_os_string();
        candidate.push(format!(" ({number})"));
        if let Some(extension) = extension {
            candidate.push(".");
            candidate.push(extension);
        }
        let path = parent.join(candidate);
        if !path.exists() {
            return path;
        }
    }
    unreachable!("ran out of file names")
}

fn strip_archive_suffix(name: &str) -> &str {
    let lower = name.to_lowercase();
    let stripped = NAME_SUFFIXES
        .iter()
        .find(|suffix| lower.ends_with(*suffix) && lower.len() > suffix.len())
        // 소문자로 바꾸어도 ASCII 확장자의 바이트 길이는 그대로입니다.
        .and_then(|suffix| name.get(..name.len() - suffix.len()))
        .unwrap_or(name);
    // 분할 RAR(`이름.part1.rar`)은 순번도 뗍니다.
    if lower.ends_with(".rar") {
        if let Some((base, tail)) = stripped.rsplit_once('.') {
            let digits = tail.get(4..).unwrap_or_default();
            let is_part = tail.get(..4).is_some_and(|word| word.eq_ignore_ascii_case("part"))
                && !digits.is_empty()
                && digits.bytes().all(|byte| byte.is_ascii_digit());
            if is_part && !base.is_empty() {
                return base;
            }
        }
    }
    stripped
}

/// gz 처럼 파일 하나만 담는 형식에서, 풀려 나올 파일의 이름.
fn inner_file_name(archive: &Path) -> String {
    let name = archive.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let lower = name.to_lowercase();
    for (suffix, replacement) in [(".tgz", ".tar"), (".tbz2", ".tar"), (".tbz", ".tar"), (".txz", ".tar"), (".tzst", ".tar")] {
        if lower.ends_with(suffix) && lower.len() > suffix.len() {
            return format!("{}{replacement}", &name[..name.len() - suffix.len()]);
        }
    }
    for suffix in [".gz", ".bz2", ".xz", ".zst"] {
        if lower.ends_with(suffix) && lower.len() > suffix.len() {
            return name[..name.len() - suffix.len()].to_owned();
        }
    }
    name
}

// ----- 안전한 경로 ----------------------------------------------------------------

/// 압축 파일 안의 이름을 풀 폴더 기준의 상대 경로로 바꿉니다.
///
/// 절대 경로, 드라이브 문자, `..`가 섞여 있어도 결과는 항상 풀 폴더 안에 머뭅니다.
/// 쓸 수 있는 부분이 하나도 없으면 `None`입니다.
fn safe_relative(name: &str) -> Option<PathBuf> {
    let mut path = PathBuf::new();
    for part in name.split(['/', '\\']).filter(|part| !matches!(*part, "" | "." | "..")) {
        path.push(safe_component(part));
    }
    (!path.as_os_str().is_empty()).then_some(path)
}

/// Windows 에서 파일 이름으로 쓸 수 없는 부분을 `_`로 바꿉니다.
fn safe_component(part: &str) -> String {
    let mut cleaned: String = part
        .chars()
        .map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c })
        .collect();
    // Windows 는 끝의 마침표와 공백을 조용히 떼어 내어 다른 이름과 겹치게 만듭니다.
    let kept = cleaned.trim_end_matches(['.', ' ']).len();
    cleaned.truncate(kept);
    if cleaned.is_empty() || is_device_name(&cleaned) {
        cleaned.insert(0, '_');
    }
    cleaned
}

/// `CON`, `NUL.txt`처럼 Windows 가 장치로 해석하는 이름.
fn is_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim_end().to_ascii_uppercase();
    match stem.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" => true,
        _ => {
            let bytes = stem.as_bytes();
            bytes.len() == 4 && matches!(&bytes[..3], b"COM" | b"LPT") && matches!(bytes[3], b'1'..=b'9')
        }
    }
}

// ----- 파일 쓰기 -----------------------------------------------------------------

enum CopyError {
    /// 압축 파일 쪽에서 읽다가 난 오류. 암호가 틀렸을 때도 여기로 옵니다.
    Read(io::Error),
    Write(anyhow::Error),
    Cancelled,
}

impl CopyError {
    /// 읽기 오류는 `name` 항목을 풀지 못했다는 일반 오류로 바꿉니다.
    fn into_extract_error(self, name: &str) -> ExtractError {
        match self {
            CopyError::Read(error) => {
                ExtractError::Failed(anyhow!(error).context(format!("압축을 풀지 못했습니다: {name}")))
            }
            CopyError::Write(error) => ExtractError::Failed(error),
            CopyError::Cancelled => ExtractError::Cancelled,
        }
    }
}

/// 풀려 나온 항목을 임시 폴더에 씁니다.
struct Sink<'a> {
    root: &'a Path,
    progress: &'a Progress,
    files: u64,
    skipped: u64,
    buffer: Vec<u8>,
}

impl<'a> Sink<'a> {
    fn new(root: &'a Path, progress: &'a Progress) -> Self {
        Self { root, progress, files: 0, skipped: 0, buffer: vec![0; COPY_BUFFER_BYTES] }
    }

    fn check_cancel(&self) -> Result<(), ExtractError> {
        if self.progress.is_cancelled() {
            return Err(ExtractError::Cancelled);
        }
        Ok(())
    }

    fn dir(&mut self, name: &str) -> Result<(), ExtractError> {
        if let Some(relative) = safe_relative(name) {
            let path = self.root.join(relative);
            fs::create_dir_all(&path)
                .with_context(|| format!("폴더를 만들지 못했습니다: {}", path.display()))?;
        }
        Ok(())
    }

    /// 링크처럼 만들지 않기로 한 항목을 셉니다. 끝난 뒤 화면에 몇 개인지 알립니다.
    fn skip(&mut self, kind: &str, name: &str) {
        log::warn!("skipping {kind}: {name}");
        self.skipped += 1;
    }

    /// 파일이 놓일 경로를 정하고 상위 폴더를 만듭니다. 쓸 수 없는 이름이면 `None`.
    fn prepare(&mut self, name: &str) -> anyhow::Result<Option<PathBuf>> {
        let Some(relative) = safe_relative(name) else {
            log::warn!("skipping entry with an unusable name: {name:?}");
            return Ok(None);
        };
        let mut path = self.root.join(relative);
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)
                .with_context(|| format!("폴더를 만들지 못했습니다: {}", dir.display()))?;
        }
        // `A.txt`와 `a.txt`, `x?.txt`와 `x_.txt`는 Windows 에서 같은 파일이 됩니다.
        // 앞서 푼 것을 덮어쓰지 않도록 번호를 붙입니다.
        if fs::symlink_metadata(&path).is_ok() {
            let renamed = match (path.parent(), path.file_name()) {
                (Some(dir), Some(file_name)) => Some(unique_path(dir, file_name, false)),
                _ => None,
            };
            if let Some(renamed) = renamed {
                path = renamed;
            }
        }
        self.progress.set_current(name);
        Ok(Some(path))
    }

    /// `reader`의 내용을 `name` 파일로 씁니다. `count_bytes`이면 쓴 만큼 진행률에 더합니다.
    fn file(
        &mut self,
        name: &str,
        reader: &mut dyn Read,
        modified: Option<SystemTime>,
        count_bytes: bool,
    ) -> Result<(), CopyError> {
        let Some(path) = self.prepare(name).map_err(CopyError::Write)? else {
            return Ok(());
        };
        let write_error = |error: io::Error| {
            CopyError::Write(anyhow!(error).context(format!("파일을 쓰지 못했습니다: {}", path.display())))
        };
        let mut out = File::create(&path).map_err(write_error)?;
        loop {
            if self.progress.is_cancelled() {
                return Err(CopyError::Cancelled);
            }
            let read = match reader.read(&mut self.buffer) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(CopyError::Read(error)),
            };
            out.write_all(&self.buffer[..read]).map_err(write_error)?;
            if count_bytes {
                self.progress.add(read as u64);
            }
        }
        if let Some(modified) = modified {
            // 수정 시각을 못 맞추는 것은 풀기 실패가 아닙니다.
            let _ = out.set_modified(modified);
        }
        self.files += 1;
        Ok(())
    }
}

// ----- zip ---------------------------------------------------------------------

fn extract_zip(path: &Path, password: Option<&str>, sink: &mut Sink<'_>) -> Result<(), ExtractError> {
    let mut zip = open_zip(path)?;
    let total: u64 = (0..zip.len())
        .filter_map(|index| zip.by_index_data(index).ok())
        .filter(|entry| !entry.is_dir())
        .fold(0, |sum, entry| sum.saturating_add(entry.size()));
    sink.progress.set_total(total);

    // 암호는 풀기 전에 확인합니다. 그러지 않으면 암호 없는 항목을 다 푼 뒤에야 묻게 됩니다.
    // 한글 암호는 만든 프로그램에 따라 UTF-8 이거나 CP949 이므로 통과하는 쪽을 씁니다.
    let first_encrypted = (0..zip.len()).find(|&index| {
        zip.by_index_data(index).is_ok_and(|entry| entry.encrypted() && !entry.is_dir() && !entry.is_symlink())
    });
    let key = match first_encrypted {
        None => None,
        Some(index) => {
            let candidates = password.map(password_candidates).ok_or(ExtractError::PasswordRequired)?;
            let working = candidates
                .into_iter()
                .find(|candidate| !matches!(zip.by_index_decrypt(index, candidate), Err(ZipError::InvalidPassword)))
                .ok_or(ExtractError::WrongPassword)?;
            Some(working)
        }
    };
    for index in 0..zip.len() {
        sink.check_cancel()?;
        let entry = zip.by_index_data(index).context("zip 항목을 읽지 못했습니다")?;
        let name = decode_name(entry.name_raw());
        let (is_dir, is_symlink, encrypted) = (entry.is_dir(), entry.is_symlink(), entry.encrypted());
        drop(entry);

        if is_dir {
            sink.dir(&name)?;
            continue;
        }
        if is_symlink {
            sink.skip("symbolic link", &name);
            continue;
        }
        if !encrypted {
            let mut file = zip.by_index(index).map_err(|error| zip_error(error, &name))?;
            let modified = zip_modified(&file);
            sink.file(&name, &mut file, modified, true)
                .map_err(|error| error.into_extract_error(&name))?;
            continue;
        }

        let key = key.as_deref().ok_or(ExtractError::PasswordRequired)?;
        let mut file = zip.by_index_decrypt(index, key).map_err(|error| zip_error(error, &name))?;
        let modified = zip_modified(&file);
        // AES 는 여는 순간 암호를 검증하므로, 그 뒤의 읽기 오류는 파일 손상입니다.
        let verified = file
            .extra_data_fields()
            .any(|field| matches!(field, zip::extra_fields::ExtraField::AeXEncryption(_)));
        sink.file(&name, &mut file, modified, true).map_err(|error| match error {
            // ZipCrypto 는 틀린 암호도 256번에 한 번은 통과시키고, 끝에서 체크섬이 어긋납니다.
            CopyError::Read(_) if !verified => ExtractError::WrongPassword,
            other => other.into_extract_error(&name),
        })?;
    }
    Ok(())
}

fn zip_error(error: ZipError, name: &str) -> ExtractError {
    match error {
        ZipError::InvalidPassword => ExtractError::WrongPassword,
        ZipError::UnsupportedArchive(ZipError::PASSWORD_REQUIRED) => ExtractError::PasswordRequired,
        other => ExtractError::Failed(anyhow!(other).context(format!("압축을 풀지 못했습니다: {name}"))),
    }
}

/// zip 항목의 수정 시각. 추가 필드의 UTC 시각이 있으면 그것을, 없으면 현지 시각으로 적힌
/// 기본 필드를 씁니다.
fn zip_modified<R: Read>(file: &zip::read::ZipFile<'_, R>) -> Option<SystemTime> {
    use zip::extra_fields::ExtraField;

    for field in file.extra_data_fields() {
        match field {
            ExtraField::Ntfs(ntfs) => return filetime_to_system_time(ntfs.mtime()),
            ExtraField::ExtendedTimestamp(stamp) => {
                if let Some(seconds) = stamp.mod_time() {
                    return unix_to_system_time(u64::from(seconds));
                }
            }
            _ => {}
        }
    }
    let stamp = file.last_modified()?;
    local_to_system_time(
        stamp.year(),
        u16::from(stamp.month()),
        u16::from(stamp.day()),
        u16::from(stamp.hour()),
        u16::from(stamp.minute()),
        u16::from(stamp.second()),
    )
}

// ----- 7z ----------------------------------------------------------------------

type SevenZReader = sevenz_rust2::ArchiveReader<File>;

fn open_7z(path: &Path, password: Option<&str>) -> Result<SevenZReader, ExtractError> {
    use sevenz_rust2::{ArchiveReader, Error, Password};

    let open = |password: Password| ArchiveReader::open(path, password);
    match open(password.map(Password::new).unwrap_or_else(Password::empty)) {
        Ok(reader) => Ok(reader),
        Err(Error::PasswordRequired) => Err(ExtractError::PasswordRequired),
        Err(Error::MaybeBadPassword(_)) => Err(ExtractError::WrongPassword),
        // 목록까지 암호화한 7z 는 틀린 암호로 열면 갖가지 오류가 납니다.
        // 암호 없이는 열리지 않는 파일이라면 암호가 틀린 것으로 봅니다.
        Err(_) if password.is_some() && matches!(open(Password::empty()), Err(Error::PasswordRequired)) => {
            Err(ExtractError::WrongPassword)
        }
        Err(error) => Err(ExtractError::Failed(anyhow!(error).context("7z 파일을 읽지 못했습니다"))),
    }
}

fn extract_7z(path: &Path, password: Option<&str>, sink: &mut Sink<'_>) -> Result<(), ExtractError> {
    use sevenz_rust2::Error;

    let mut reader = open_7z(path, password)?;
    let archive = reader.archive();
    let encrypted = (0..archive.files.len()).any(|index| archive.is_file_encrypted(index));
    let total = archive
        .files
        .iter()
        .filter(|entry| !entry.is_directory())
        .fold(0u64, |sum, entry| sum.saturating_add(entry.size()));
    sink.progress.set_total(total);

    // 콜백 안에서 난 우리 쪽 오류는 따로 들고 나와, 압축 파일 쪽 오류와 구분합니다.
    let mut failure: Option<ExtractError> = None;
    let result = reader.for_each_entries(|entry, data| {
        let name = entry.name().replace('\\', "/");
        let step = if sink.progress.is_cancelled() {
            Err(ExtractError::Cancelled)
        } else if entry.is_directory() {
            sink.dir(&name)
        } else {
            let modified = entry.has_last_modified_date.then(|| entry.last_modified_date().into());
            match sink.file(&name, data, modified, true) {
                Ok(()) => Ok(()),
                // 읽기 오류는 라이브러리가 더 자세한 오류로 돌려주도록 그대로 넘깁니다.
                Err(CopyError::Read(error)) => return Err(Error::from(error)),
                Err(other) => Err(other.into_extract_error(&name)),
            }
        };
        match step {
            Ok(()) => Ok(true),
            Err(error) => {
                failure = Some(error);
                Err(Error::Other("stopped".into()))
            }
        }
    });

    match (failure, result) {
        (Some(failure), _) => Err(failure),
        (None, Ok(())) => Ok(()),
        (None, Err(Error::PasswordRequired)) => Err(ExtractError::PasswordRequired),
        (None, Err(Error::MaybeBadPassword(_))) => Err(ExtractError::WrongPassword),
        // 틀린 암호로 풀면 쓰레기 데이터가 나와 압축 해제나 체크섬 검사에서 걸립니다.
        (None, Err(_)) if encrypted && password.is_some() => Err(ExtractError::WrongPassword),
        (None, Err(error)) => Err(ExtractError::Failed(anyhow!(error).context("7z 압축을 풀지 못했습니다"))),
    }
}

// ----- rar ---------------------------------------------------------------------

/// 분할 압축의 중간 파일을 열었으면 첫 파일부터 풀도록 바꿉니다.
fn rar_first_volume(path: &Path) -> PathBuf {
    let first = unrar::Archive::new(path).as_first_part().filename().to_path_buf();
    if first.is_file() {
        first
    } else {
        path.to_path_buf()
    }
}

fn rar_archive<'a>(path: &'a Path, password: Option<&'a [u8]>) -> unrar::Archive<'a> {
    match password {
        Some(password) => unrar::Archive::with_password(path, password),
        None => unrar::Archive::new(path),
    }
}

/// unrar 는 암호를 시스템 코드 페이지의 바이트열로 받습니다. NUL 은 넘길 수 없습니다.
fn rar_password(password: &str) -> Vec<u8> {
    let password: String = password.chars().filter(|c| *c != '\0').collect();
    if password.is_ascii() || !system_code_page_is_korean() {
        return password.into_bytes();
    }
    encoding_rs::EUC_KR.encode(&password).0.into_owned()
}

/// `encrypted`는 틀린 암호로도 날 수 있는 손상 오류를 암호 오류로 볼지를 정합니다.
fn rar_error(error: unrar::error::UnrarError, encrypted: bool, has_password: bool) -> ExtractError {
    use unrar::error::Code;

    match error.code {
        Code::MissingPassword => ExtractError::PasswordRequired,
        Code::BadPassword => ExtractError::WrongPassword,
        // RAR4 는 암호를 검사하지 않아, 틀린 암호는 손상된 데이터로 나타납니다.
        Code::BadData | Code::BadArchive | Code::UnknownFormat if encrypted && has_password => {
            ExtractError::WrongPassword
        }
        _ => ExtractError::Failed(anyhow!("RAR 파일을 읽지 못했습니다: {error}")),
    }
}

fn extract_rar(path: &Path, password: Option<&str>, sink: &mut Sink<'_>) -> Result<(), ExtractError> {
    let path = rar_first_volume(path);
    let total = list_rar(&path, password)?.total_bytes;
    sink.progress.set_total(total);

    let password = password.map(rar_password);
    let has_password = password.is_some();
    let mut archive = rar_archive(&path, password.as_deref())
        .open_for_processing()
        .map_err(|error| rar_error(error, false, has_password))?;
    let encrypted_headers = archive.has_encrypted_headers();
    loop {
        sink.check_cancel()?;
        let Some(header) = archive
            .read_header()
            .map_err(|error| rar_error(error, encrypted_headers, has_password))?
        else {
            return Ok(());
        };
        let entry = header.entry();
        let name = entry.filename.to_string_lossy().replace('\\', "/");
        let (size, encrypted) = (entry.unpacked_size, entry.is_encrypted() || encrypted_headers);
        let skip_error = |error| rar_error(error, encrypted, has_password);

        if entry.is_directory() {
            sink.dir(&name)?;
            archive = header.skip().map_err(skip_error)?;
            continue;
        }
        let Some(target) = sink.prepare(&name)? else {
            archive = header.skip().map_err(skip_error)?;
            continue;
        };
        // unrar 가 파일을 직접 쓰므로 진행률은 파일 단위로만 올라갑니다.
        archive = header.extract_to(&target).map_err(|error| match rar_error(error, encrypted, has_password) {
            ExtractError::Failed(error) => ExtractError::Failed(error.context(format!("압축을 풀지 못했습니다: {name}"))),
            other => other,
        })?;
        // unrar 는 링크 항목을 스스로 만듭니다. 풀 폴더 밖을 가리킬 수 있으므로 남기지 않습니다.
        if fs::symlink_metadata(&target).is_ok_and(|meta| meta.file_type().is_symlink()) {
            let _ = fs::remove_file(&target).or_else(|_| fs::remove_dir(&target));
            sink.skip("link", &name);
            continue;
        }
        sink.progress.add(size);
        sink.files += 1;
    }
}

// ----- tar 와 압축 스트림 ---------------------------------------------------------

/// 읽은 압축 바이트 수를 진행률에 더합니다.
struct CountingReader<'a> {
    inner: File,
    progress: &'a Progress,
}

impl Read for CountingReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.progress.add(read as u64);
        Ok(read)
    }
}

fn extract_stream(path: &Path, format: Format, sink: &mut Sink<'_>) -> Result<(), ExtractError> {
    let file = File::open(path).with_context(|| format!("cannot open file: {}", path.display()))?;
    // 풀린 크기는 끝까지 풀어야 알 수 있으므로, 압축 파일을 읽은 양으로 진행률을 잽니다.
    sink.progress.set_total(file.metadata().map(|meta| meta.len()).unwrap_or(0));
    let source = BufReader::with_capacity(COPY_BUFFER_BYTES, CountingReader { inner: file, progress: sink.progress });

    let mut decoded: Box<dyn Read + '_> = match format {
        Format::Gzip => Box::new(GzMembers::new(source)),
        Format::Bzip2 => Box::new(bzip2::bufread::MultiBzDecoder::new(source)),
        Format::Xz => Box::new(lzma_rust2::XzReader::new(source, true)),
        Format::Zstd => Box::new(
            zstd::stream::read::Decoder::with_buffer(source).context("zst 파일을 읽지 못했습니다")?,
        ),
        _ => Box::new(source),
    };

    let mut head = vec![0u8; TAR_BLOCK];
    let filled = read_up_to(&mut decoded, &mut head)
        .with_context(|| format!("{} 압축을 풀지 못했습니다", format.label()))?;
    head.truncate(filled);
    let is_tar = is_tar_header(&head);
    let mut content = Cursor::new(head).chain(decoded);

    if is_tar {
        untar(&mut content, sink)?;
        // tar 는 끝 표식에서 읽기를 멈춥니다. 압축 스트림의 체크섬은 끝까지 읽어야 검사되므로,
        // 남은 부분을 마저 읽어 손상된 파일을 성공으로 넘기지 않게 합니다.
        return if format == Format::Tar { Ok(()) } else { drain(&mut content, sink, format) };
    }
    if format == Format::Tar {
        return Err(anyhow!("tar 파일이 손상되어 읽을 수 없습니다").into());
    }
    let name = inner_file_name(path);
    sink.file(&name, &mut content, None, false)
        .map_err(|error| error.into_extract_error(&name))
}

fn drain(content: &mut dyn Read, sink: &mut Sink<'_>, format: Format) -> Result<(), ExtractError> {
    loop {
        sink.check_cancel()?;
        match content.read(&mut sink.buffer) {
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                let context = format!("{} 압축 파일이 손상되었습니다", format.label());
                return Err(anyhow!(error).context(context).into());
            }
        }
    }
}

/// 이어 붙인 gzip 을 차례로 풉니다. `MultiGzDecoder` 와 달리, gzip(1) 처럼 끝에 붙은 0 채움은
/// 무시합니다.
struct GzMembers<R: io::BufRead> {
    decoder: Option<flate2::bufread::GzDecoder<R>>,
}

impl<R: io::BufRead> GzMembers<R> {
    fn new(source: R) -> Self {
        Self { decoder: Some(flate2::bufread::GzDecoder::new(source)) }
    }
}

impl<R: io::BufRead> Read for GzMembers<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            let Some(mut decoder) = self.decoder.take() else {
                return Ok(0);
            };
            match decoder.read(buffer) {
                Ok(0) if !buffer.is_empty() => {
                    // 한 덩어리가 끝났습니다. 뒤에 0 이 아닌 데이터가 있으면 다음 덩어리입니다.
                    let mut source = decoder.into_inner();
                    if skip_zeros(&mut source)? {
                        self.decoder = Some(flate2::bufread::GzDecoder::new(source));
                    }
                }
                result => {
                    self.decoder = Some(decoder);
                    return result;
                }
            }
        }
    }
}

/// 0 을 건너뜁니다. 그 뒤에 데이터가 남아 있으면 `true`.
fn skip_zeros(source: &mut impl io::BufRead) -> io::Result<bool> {
    loop {
        let available = source.fill_buf()?;
        if available.is_empty() {
            return Ok(false);
        }
        let zeros = available.iter().take_while(|byte| **byte == 0).count();
        let more = zeros < available.len();
        source.consume(zeros);
        if more {
            return Ok(true);
        }
    }
}

fn untar(content: &mut dyn Read, sink: &mut Sink<'_>) -> Result<(), ExtractError> {
    let mut archive = tar::Archive::new(content);
    for entry in archive.entries().context("tar 파일을 읽지 못했습니다")? {
        sink.check_cancel()?;
        let mut entry = entry.context("tar 파일을 읽지 못했습니다")?;
        let name = decode_name(&entry.path_bytes());
        let kind = entry.header().entry_type();
        if is_tar_dir(kind, &name) {
            sink.dir(&name)?;
        } else if is_tar_file(kind) {
            let modified = entry.header().mtime().ok().and_then(unix_to_system_time);
            sink.file(&name, &mut entry, modified, false)
                .map_err(|error| error.into_extract_error(&name))?;
        } else {
            // 링크와 장치 파일은 풀 폴더 밖을 가리킬 수 있어 만들지 않습니다.
            sink.skip(&format!("{kind:?} entry"), &name);
        }
    }
    Ok(())
}

/// 옛 tar(v7)는 폴더에 따로 종류 표시가 없고, 이름 끝의 `/`로만 구분합니다.
fn is_tar_dir(kind: tar::EntryType, name: &str) -> bool {
    kind.is_dir() || (kind.is_file() && name.ends_with('/'))
}

fn is_tar_file(kind: tar::EntryType) -> bool {
    kind.is_file() || kind.is_contiguous() || kind.is_gnu_sparse()
}

// ----- 시각 변환 -----------------------------------------------------------------

fn unix_to_system_time(seconds: u64) -> Option<SystemTime> {
    UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
}

/// Windows FILETIME(1601년부터의 100나노초 단위)을 바꿉니다.
fn filetime_to_system_time(ticks: u64) -> Option<SystemTime> {
    const EPOCH_DIFFERENCE_SECONDS: u64 = 11_644_473_600;
    unix_to_system_time((ticks / 10_000_000).checked_sub(EPOCH_DIFFERENCE_SECONDS)?)
}

fn utc_to_system_time(year: u16, month: u16, day: u16, hour: u16, minute: u16, second: u16) -> Option<SystemTime> {
    // 그레고리력 날짜를 1970-01-01 부터의 날수로 바꿉니다.
    let (year, month, day) = (i64::from(year), i64::from(month), i64::from(day));
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    let seconds = days * 86_400 + i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second);
    unix_to_system_time(u64::try_from(seconds).ok()?)
}

/// 이 PC 의 시간대로 적힌 시각을 바꿉니다. 서머타임도 그 날짜 기준으로 반영됩니다.
#[cfg(windows)]
fn local_to_system_time(year: u16, month: u16, day: u16, hour: u16, minute: u16, second: u16) -> Option<SystemTime> {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::Time::TzSpecificLocalTimeToSystemTime;

    let local = SYSTEMTIME {
        wYear: year,
        wMonth: month,
        wDayOfWeek: 0,
        wDay: day,
        wHour: hour,
        wMinute: minute,
        wSecond: second,
        wMilliseconds: 0,
    };
    let mut utc = SYSTEMTIME { ..local };
    // SAFETY: 두 포인터는 이 함수 안에 살아 있는 구조체를 가리키고, 시간대 인자가 null 이면
    // 현재 시간대를 쓴다고 문서에 정해져 있습니다.
    let converted = unsafe { TzSpecificLocalTimeToSystemTime(std::ptr::null(), &local, &mut utc) };
    if converted == 0 {
        return None;
    }
    utc_to_system_time(utc.wYear, utc.wMonth, utc.wDay, utc.wHour, utc.wMinute, utc.wSecond)
}

#[cfg(not(windows))]
fn local_to_system_time(year: u16, month: u16, day: u16, hour: u16, minute: u16, second: u16) -> Option<SystemTime> {
    utc_to_system_time(year, month, day, hour, minute, second)
}

#[cfg(windows)]
fn system_code_page_is_korean() -> bool {
    // SAFETY: 인자가 없고 실패하지 않는 함수입니다.
    unsafe { windows_sys::Win32::Globalization::GetACP() == 949 }
}

#[cfg(not(windows))]
fn system_code_page_is_korean() -> bool {
    false
}

#[cfg(test)]
pub(crate) mod test_support {
    use zip::unstable::write::FileOptionsExt as _;
    use zip::write::SimpleFileOptions;
    use zip::{AesMode, ZipWriter};

    use super::*;

    /// 테스트마다 따로 쓰는 임시 폴더. 끝나면 지웁니다.
    pub struct Workspace(pub PathBuf);

    impl Workspace {
        pub fn new(label: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("aspartame-extract-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        pub fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, bytes).unwrap();
            path
        }

        /// 폴더 안의 이름을 정렬해 돌려줍니다.
        pub fn names(&self) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(&self.0)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for Workspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    pub fn unpack(archive: &Path, password: Option<&str>) -> Result<Extracted, ExtractError> {
        let format = detect(archive).expect("format not detected");
        extract_into(archive, format, archive.parent().unwrap(), password, &Progress::default())
    }

    pub fn text(path: &Path) -> String {
        fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
    }

    /// 모든 항목에 같은 암호를 건 zip. `aes`가 아니면 옛 ZipCrypto 방식입니다.
    pub fn encrypted_zip(files: &[(&str, &[u8])], aes: bool, password: &[u8]) -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in files {
            let options = SimpleFileOptions::default();
            let options = if aes {
                options.with_aes_encryption(AesMode::Aes256, std::str::from_utf8(password).unwrap())
            } else {
                options.with_deprecated_encryption(password).unwrap()
            };
            writer.start_file(*name, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    pub fn seven_zip(files: &[(&str, &[u8])], password: Option<&str>, encrypt_names: bool) -> Vec<u8> {
        use sevenz_rust2::encoder_options::AesEncoderOptions;
        use sevenz_rust2::{ArchiveEntry, ArchiveWriter, EncoderMethod, Password};

        let mut writer = ArchiveWriter::new(Cursor::new(Vec::new())).unwrap();
        if let Some(password) = password {
            writer.set_content_methods(vec![
                AesEncoderOptions::new(Password::new(password)).into(),
                EncoderMethod::LZMA2.into(),
            ]);
            writer.set_encrypt_header(encrypt_names);
        }
        for (name, bytes) in files {
            writer.push_archive_entry(ArchiveEntry::new_file(name), Some(*bytes)).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }
}

#[cfg(test)]
mod tests {
    use zip::unstable::write::FileOptionsExt as _;
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    use super::test_support::{encrypted_zip, seven_zip, text, unpack, Workspace};
    use super::*;
    use crate::archive::test_support::build_zip;

    fn tarball(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, bytes) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(1_600_000_000);
            builder.append_data(&mut header, name, *bytes).unwrap();
        }
        builder.into_inner().unwrap()
    }

    #[test]
    fn detects_formats_by_content() {
        let tar = tarball(&[("a.txt", b"x")]);
        let cases: [(&[u8], Option<Format>); 10] = [
            (b"PK\x03\x04rest", Some(Format::Zip)),
            (b"7z\xBC\xAF\x27\x1Crest", Some(Format::SevenZ)),
            (b"Rar!\x1A\x07\x01\x00", Some(Format::Rar)),
            (b"\x1F\x8B\x08\x00", Some(Format::Gzip)),
            (b"BZh91AY&SY", Some(Format::Bzip2)),
            (b"\xFD7zXZ\x00\x00", Some(Format::Xz)),
            (b"\x28\xB5\x2F\xFD\x00", Some(Format::Zstd)),
            (&tar, Some(Format::Tar)),
            (b"hello world", None),
            (&[0u8; 1024], None),
        ];
        for (bytes, expected) in cases {
            assert_eq!(detect_bytes(bytes), expected, "{:?}", &bytes[..bytes.len().min(8)]);
        }
        assert_eq!(detect(Path::new("no-such-file.zip")), None);
    }

    #[test]
    fn unsafe_names_stay_inside_the_folder() {
        let cases = [
            ("../../evil.txt", "evil.txt"),
            ("/etc/passwd", "etc/passwd"),
            ("C:\\Windows\\evil.dll", "C_/Windows/evil.dll"),
            ("a/./../b.txt", "a/b.txt"),
            ("dir/name. ", "dir/name"),
            ("con.txt", "_con.txt"),
            ("LPT1", "_LPT1"),
            ("com10.txt", "com10.txt"),
            // 글자 중간에서 자르면 안 되는 이름.
            ("a한글.txt", "a한글.txt"),
            ("가나", "가나"),
            ("what?.txt", "what_.txt"),
            ("...", "_"),
        ];
        for (name, expected) in cases {
            let relative = safe_relative(name).unwrap();
            assert_eq!(relative, PathBuf::from(expected), "{name}");
        }
        assert_eq!(safe_relative("../.."), None);
        assert_eq!(safe_relative(""), None);
    }

    #[test]
    fn several_top_level_items_go_into_a_folder_named_after_the_archive() {
        let work = Workspace::new("folder");
        let archive = work.file("묶음.zip", &build_zip(&[("a.txt", b"one"), ("sub/b.txt", b"two")]));

        let first = unpack(&archive, None).unwrap();
        assert_eq!(first.target, work.0.join("묶음"));
        assert_eq!(first.files, 2);
        assert_eq!(text(&first.target.join("a.txt")), "one");
        assert_eq!(text(&first.target.join("sub").join("b.txt")), "two");

        // 같은 이름의 폴더가 있으면 덮어쓰지 않고 번호를 붙입니다.
        let second = unpack(&archive, None).unwrap();
        assert_eq!(second.target, work.0.join("묶음 (2)"));
        assert_eq!(work.names(), vec!["묶음", "묶음 (2)", "묶음.zip"]);
    }

    #[test]
    fn a_single_top_level_item_is_not_wrapped_in_another_folder() {
        let work = Workspace::new("single");
        let folder = work.file("album.zip", &build_zip(&[("photos/a.txt", b"a"), ("photos/b.txt", b"b")]));
        let file = work.file("note.zip", &build_zip(&[("note.v2.txt", b"hello")]));

        assert_eq!(unpack(&folder, None).unwrap().target, work.0.join("photos"));
        assert_eq!(unpack(&file, None).unwrap().target, work.0.join("note.v2.txt"));
        assert_eq!(unpack(&file, None).unwrap().target, work.0.join("note.v2 (2).txt"));
        assert_eq!(text(&work.0.join("photos").join("b.txt")), "b");
        assert_eq!(
            work.names(),
            vec!["album.zip", "note.v2 (2).txt", "note.v2.txt", "note.zip", "photos"]
        );
    }

    #[test]
    fn zip_entries_cannot_escape_the_target_folder() {
        let work = Workspace::new("slip");
        let nested = work.0.join("nested");
        fs::create_dir(&nested).unwrap();
        let archive = nested.join("evil.zip");
        let entries: [(&str, &[u8]); 4] =
            [("../outside.txt", b"1"), ("/abs.txt", b"2"), ("C:/drive.txt", b"3"), ("ok.txt", b"4")];
        fs::write(&archive, build_zip(&entries)).unwrap();

        let extracted = unpack(&archive, None).unwrap();
        assert_eq!(extracted.target, nested.join("evil"));
        assert_eq!(extracted.files, 4);
        assert_eq!(text(&extracted.target.join("outside.txt")), "1");
        assert_eq!(text(&extracted.target.join("C_").join("drive.txt")), "3");
        assert_eq!(work.names(), vec!["nested"], "something was written outside the target");
    }

    #[test]
    fn encrypted_zips_ask_for_the_password_and_reject_a_wrong_one() {
        let work = Workspace::new("zip-password");
        for (label, aes) in [("zipcrypto", false), ("aes", true)] {
            let bytes = encrypted_zip(&[("secret.txt", b"top secret")], aes, b"open sesame");
            let archive = work.file(&format!("{label}.zip"), &bytes);

            let listing = list(&archive, Format::Zip, None).unwrap().unwrap();
            assert!(listing.encrypted, "{label}");
            assert!(matches!(unpack(&archive, None), Err(ExtractError::PasswordRequired)), "{label}");
            assert!(matches!(unpack(&archive, Some("nope")), Err(ExtractError::WrongPassword)), "{label}");
            // 실패한 시도는 아무것도 남기지 않습니다.
            assert_eq!(work.names(), vec![format!("{label}.zip")], "{label}");

            let extracted = unpack(&archive, Some("open sesame")).unwrap();
            assert_eq!(text(&extracted.target), "top secret", "{label}");
            fs::remove_file(&extracted.target).unwrap();
            fs::remove_file(&archive).unwrap();
        }
    }

    #[test]
    fn korean_zip_passwords_work_in_both_encodings() {
        let work = Workspace::new("zip-korean");
        let cp949 = encoding_rs::EUC_KR.encode("비밀번호").0.into_owned();
        for (label, password) in [("utf8", "비밀번호".as_bytes().to_vec()), ("cp949", cp949)] {
            let archive = work.file(
                &format!("{label}.zip"),
                &encrypted_zip(&[(&format!("{label}.txt"), b"contents")], false, &password),
            );
            let extracted = unpack(&archive, Some("비밀번호")).unwrap();
            assert_eq!(text(&extracted.target), "contents", "{label}");
        }
    }

    #[test]
    fn seven_zip_archives_extract_with_and_without_a_password() {
        let work = Workspace::new("7z");
        let files: [(&str, &[u8]); 2] = [("docs/readme.txt", b"hello 7z"), ("docs/데이터.bin", &[7u8; 5000])];

        let plain = work.file("plain.7z", &seven_zip(&files, None, false));
        let listing = list(&plain, Format::SevenZ, None).unwrap().unwrap();
        assert_eq!((listing.files, listing.total_bytes, listing.encrypted), (2, 5008, false));
        let extracted = unpack(&plain, None).unwrap();
        assert_eq!(extracted.target, work.0.join("docs"));
        assert_eq!(text(&extracted.target.join("readme.txt")), "hello 7z");
        assert_eq!(fs::read(extracted.target.join("데이터.bin")).unwrap(), vec![7u8; 5000]);
        fs::remove_dir_all(&extracted.target).unwrap();

        // 내용만 암호화: 목록은 보이지만 풀 때 암호가 필요합니다.
        let locked = work.file("locked.7z", &seven_zip(&files, Some("열려라"), false));
        assert!(list(&locked, Format::SevenZ, None).unwrap().unwrap().encrypted);
        assert!(matches!(unpack(&locked, None), Err(ExtractError::PasswordRequired)));
        assert!(matches!(unpack(&locked, Some("wrong")), Err(ExtractError::WrongPassword)));
        let extracted = unpack(&locked, Some("열려라")).unwrap();
        assert_eq!(text(&extracted.target.join("readme.txt")), "hello 7z");
        fs::remove_dir_all(&extracted.target).unwrap();

        // 목록까지 암호화: 암호 없이는 목록도 읽을 수 없습니다.
        let hidden = work.file("hidden.7z", &seven_zip(&files, Some("pw"), true));
        assert!(matches!(list(&hidden, Format::SevenZ, None), Err(ExtractError::PasswordRequired)));
        assert!(matches!(list(&hidden, Format::SevenZ, Some("bad")), Err(ExtractError::WrongPassword)));
        assert_eq!(list(&hidden, Format::SevenZ, Some("pw")).unwrap().unwrap().files, 2);
        assert!(matches!(unpack(&hidden, Some("bad")), Err(ExtractError::WrongPassword)));
        assert_eq!(unpack(&hidden, Some("pw")).unwrap().files, 2);
        assert_eq!(work.names(), vec!["docs", "hidden.7z", "locked.7z", "plain.7z"]);
    }

    /// RAR 은 만들 수 없으므로 unrar 크레이트(MIT/Apache-2.0)에 들어 있는 견본을 씁니다.
    #[test]
    fn rar_archives_extract_with_and_without_a_password() {
        let work = Workspace::new("rar");

        let plain = work.file("plain.rar", include_bytes!("../tests/data/version.rar"));
        let listing = list(&plain, Format::Rar, None).unwrap().unwrap();
        assert_eq!(listing.files, 1);
        assert_eq!(listing.entries[0].name, "VERSION");
        let extracted = unpack(&plain, None).unwrap();
        assert_eq!(extracted.target, work.0.join("VERSION"));
        assert_eq!(fs::metadata(&extracted.target).unwrap().len(), listing.total_bytes);

        // 내용만 암호화한 RAR.
        let locked = work.file("locked.rar", include_bytes!("../tests/data/crypted.rar"));
        assert!(list(&locked, Format::Rar, None).unwrap().unwrap().encrypted);
        assert!(matches!(unpack(&locked, None), Err(ExtractError::PasswordRequired)));
        assert!(matches!(unpack(&locked, Some("wrong")), Err(ExtractError::WrongPassword)));
        let extracted = unpack(&locked, Some("unrar")).unwrap();
        assert_eq!(text(&extracted.target), "target\nCargo.lock\n");
        fs::remove_file(&extracted.target).unwrap();

        // 목록까지 암호화한 RAR.
        let hidden = work.file("hidden.rar", include_bytes!("../tests/data/comment-hpw-password.rar"));
        assert!(matches!(list(&hidden, Format::Rar, None), Err(ExtractError::PasswordRequired)));
        assert!(matches!(list(&hidden, Format::Rar, Some("wrong")), Err(ExtractError::WrongPassword)));
        assert_eq!(list(&hidden, Format::Rar, Some("password")).unwrap().unwrap().files, 1);
        assert!(matches!(unpack(&hidden, None), Err(ExtractError::PasswordRequired)));
        let extracted = unpack(&hidden, Some("password")).unwrap();
        assert_eq!(text(&extracted.target), "target\nCargo.lock\n");
        fs::remove_file(&extracted.target).unwrap();

        assert_eq!(work.names(), vec!["VERSION", "hidden.rar", "locked.rar", "plain.rar"]);
    }

    #[test]
    fn tar_archives_extract_through_every_compression() {
        let work = Workspace::new("tar");
        let tar = tarball(&[("pkg/a.txt", b"alpha"), ("pkg/deep/b.txt", b"beta")]);

        let gz = {
            let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder.write_all(&tar).unwrap();
            encoder.finish().unwrap()
        };
        let bz2 = {
            let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
            encoder.write_all(&tar).unwrap();
            encoder.finish().unwrap()
        };
        let xz = {
            let options = lzma_rust2::XzOptions::with_preset(1);
            let mut encoder = lzma_rust2::XzWriter::new(Vec::new(), options).unwrap();
            encoder.write_all(&tar).unwrap();
            encoder.finish().unwrap()
        };
        let zst = zstd::encode_all(tar.as_slice(), 1).unwrap();

        let listing = list(&work.file("list.tar", &tar), Format::Tar, None).unwrap().unwrap();
        assert_eq!((listing.files, listing.total_bytes), (2, 9));

        let cases: [(&str, &[u8], Format); 5] = [
            ("plain.tar", &tar, Format::Tar),
            ("gzip.tar.gz", &gz, Format::Gzip),
            ("bzip.tbz2", &bz2, Format::Bzip2),
            ("xz.tar.xz", &xz, Format::Xz),
            ("zstd.tar.zst", &zst, Format::Zstd),
        ];
        for (name, bytes, format) in cases {
            let archive = work.file(name, bytes);
            assert_eq!(detect(&archive), Some(format), "{name}");
            let extracted = unpack(&archive, None).unwrap_or_else(|error| panic!("{name}: {error:?}"));
            assert_eq!(extracted.target, work.0.join("pkg"), "{name}");
            assert_eq!(extracted.files, 2, "{name}");
            assert_eq!(text(&extracted.target.join("a.txt")), "alpha", "{name}");
            assert_eq!(text(&extracted.target.join("deep").join("b.txt")), "beta", "{name}");
            // tar 에 적힌 수정 시각을 그대로 옮깁니다.
            let modified = fs::metadata(extracted.target.join("a.txt")).unwrap().modified().unwrap();
            assert_eq!(modified, UNIX_EPOCH + Duration::from_secs(1_600_000_000), "{name}");
            fs::remove_dir_all(&extracted.target).unwrap();
        }
    }

    #[test]
    fn a_compressed_single_file_keeps_its_inner_name() {
        let work = Workspace::new("gz-single");
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(b"just text").unwrap();
        let archive = work.file("report.txt.gz", &encoder.finish().unwrap());

        assert!(list(&archive, Format::Gzip, None).unwrap().is_none());
        let extracted = unpack(&archive, None).unwrap();
        assert_eq!(extracted.target, work.0.join("report.txt"));
        assert_eq!(text(&extracted.target), "just text");
    }

    #[test]
    fn tar_links_are_skipped_instead_of_created() {
        let work = Workspace::new("tar-link");
        let mut builder = tar::Builder::new(Vec::new());
        let mut link = tar::Header::new_gnu();
        link.set_entry_type(tar::EntryType::Symlink);
        link.set_size(0);
        builder.append_link(&mut link, "escape", "../../outside").unwrap();
        let mut file = tar::Header::new_gnu();
        file.set_size(2);
        file.set_mode(0o644);
        builder.append_data(&mut file, "real.txt", &b"ok"[..]).unwrap();
        let archive = work.file("links.tar", &builder.into_inner().unwrap());

        let extracted = unpack(&archive, None).unwrap();
        assert_eq!(extracted.target, work.0.join("real.txt"));
        assert_eq!((extracted.files, extracted.skipped), (1, 1));
        assert_eq!(work.names(), vec!["links.tar", "real.txt"]);
    }

    #[test]
    fn cancelling_or_failing_leaves_nothing_behind() {
        let work = Workspace::new("cancel");
        let archive = work.file("big.zip", &build_zip(&[("a.txt", b"one"), ("b.txt", b"two")]));
        let progress = Progress::default();
        progress.cancel();
        let result = extract_into(&archive, Format::Zip, &work.0, None, &progress);
        assert!(matches!(result, Err(ExtractError::Cancelled)));

        let broken = work.file("broken.7z", b"7z\xBC\xAF\x27\x1C this is not a real archive");
        assert!(matches!(unpack(&broken, None), Err(ExtractError::Failed(_))));
        let empty = work.file("empty.zip", &build_zip(&[]));
        assert!(matches!(unpack(&empty, None), Err(ExtractError::Failed(_))));
        assert_eq!(work.names(), vec!["big.zip", "broken.7z", "empty.zip"]);
    }

    #[test]
    fn progress_reaches_the_total() {
        let work = Workspace::new("progress");
        let archive = work.file("data.zip", &build_zip(&[("a.bin", &[1u8; 70_000]), ("b.bin", &[2u8; 30_000])]));
        let progress = Progress::default();
        assert_eq!(progress.fraction(), None);
        extract_into(&archive, Format::Zip, &work.0, None, &progress).unwrap();
        assert_eq!(progress.fraction(), Some(1.0));
        assert_eq!(progress.current(), "b.bin");
    }

    #[test]
    fn archive_suffixes_are_stripped_from_names() {
        assert_eq!(strip_archive_suffix("사진 모음.ZIP"), "사진 모음");
        assert_eq!(strip_archive_suffix("src.tar.gz"), "src");
        assert_eq!(strip_archive_suffix("src.tgz"), "src");
        assert_eq!(strip_archive_suffix("readme"), "readme");
        assert_eq!(strip_archive_suffix(".zip"), ".zip");
        assert_eq!(strip_archive_suffix("영상.part2.rar"), "영상");
        assert_eq!(strip_archive_suffix("notes.part2.zip"), "notes.part2");
        assert_eq!(strip_archive_suffix("partition.rar"), "partition");
        assert_eq!(inner_file_name(Path::new("a/log.txt.GZ")), "log.txt");
        assert_eq!(inner_file_name(Path::new("backup.tgz")), "backup.tar");
        assert_eq!(inner_file_name(Path::new("blob")), "blob");
    }

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn a_damaged_compressed_tar_is_not_reported_as_extracted() {
        let work = Workspace::new("tar-damaged");
        let mut bytes = gzip(&tarball(&[("pkg/a.txt", b"alpha")]));
        // gzip 끝의 체크섬을 깨뜨립니다. tar 의 끝 표식보다 뒤에 있어 끝까지 읽어야 드러납니다.
        let checksum = bytes.len() - 8;
        bytes[checksum] ^= 0xFF;
        let archive = work.file("damaged.tar.gz", &bytes);

        assert!(matches!(unpack(&archive, None), Err(ExtractError::Failed(_))));
        assert_eq!(work.names(), vec!["damaged.tar.gz"]);
    }

    #[test]
    fn gzip_members_are_joined_and_trailing_zero_padding_is_ignored() {
        let work = Workspace::new("gz-padding");
        let mut joined = gzip(b"first ");
        joined.extend_from_slice(&gzip(b"second"));
        joined.extend_from_slice(&[0u8; 1024]);
        let single = work.file("joined.txt.gz", &joined);
        assert_eq!(text(&unpack(&single, None).unwrap().target), "first second");

        let mut padded = gzip(&tarball(&[("pkg/a.txt", b"alpha"), ("pkg/b.txt", b"beta")]));
        padded.extend_from_slice(&[0u8; 512]);
        let tar = work.file("padded.tar.gz", &padded);
        let extracted = unpack(&tar, None).unwrap();
        assert_eq!(text(&extracted.target.join("b.txt")), "beta");
    }

    #[test]
    fn folders_in_old_style_tars_are_created_as_folders() {
        let work = Workspace::new("tar-v7");
        let mut builder = tar::Builder::new(Vec::new());
        // 옛 tar 는 폴더도 일반 파일로 표시하고 이름 끝에 `/`만 붙입니다.
        let mut folder = tar::Header::new_old();
        folder.set_path("data/").unwrap();
        folder.set_size(0);
        folder.set_mode(0o755);
        folder.set_cksum();
        builder.append(&folder, io::empty()).unwrap();
        let mut file = tar::Header::new_old();
        file.set_path("data/a.txt").unwrap();
        file.set_size(2);
        file.set_mode(0o644);
        file.set_cksum();
        builder.append(&file, &b"ok"[..]).unwrap();
        let archive = work.file("old.tar", &builder.into_inner().unwrap());

        assert_eq!(detect(&archive), Some(Format::Tar));
        let listing = list(&archive, Format::Tar, None).unwrap().unwrap();
        assert_eq!((listing.files, listing.entries.len()), (1, 2));
        let extracted = unpack(&archive, None).unwrap();
        assert_eq!(extracted.target, work.0.join("data"));
        assert_eq!(text(&extracted.target.join("a.txt")), "ok");
    }

    #[test]
    fn names_that_become_the_same_file_on_windows_are_all_kept() {
        let work = Workspace::new("collide");
        let entries: [(&str, &[u8]); 4] =
            [("A.txt", b"upper"), ("a.txt", b"lower"), ("x?.txt", b"question"), ("x_.txt", b"underscore")];
        let archive = work.file("same.zip", &build_zip(&entries));

        let extracted = unpack(&archive, None).unwrap();
        assert_eq!(extracted.files, 4);
        let mut contents: Vec<String> = fs::read_dir(&extracted.target)
            .unwrap()
            .map(|entry| text(&entry.unwrap().path()))
            .collect();
        contents.sort();
        assert_eq!(contents, vec!["lower", "question", "underscore", "upper"]);
    }

    #[test]
    fn the_zip_password_is_checked_before_anything_is_extracted() {
        let work = Workspace::new("zip-late");
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        writer.start_file("first.bin", SimpleFileOptions::default()).unwrap();
        writer.write_all(&[5u8; 50_000]).unwrap();
        let locked = SimpleFileOptions::default().with_deprecated_encryption(b"pw").unwrap();
        writer.start_file("last.txt", locked).unwrap();
        writer.write_all(b"secret").unwrap();
        let archive = work.file("late.zip", &writer.finish().unwrap().into_inner());

        for (password, wrong) in [(None, false), (Some("nope"), true)] {
            let progress = Progress::default();
            let result = extract_into(&archive, Format::Zip, &work.0, password, &progress);
            match (result, wrong) {
                (Err(ExtractError::PasswordRequired), false) | (Err(ExtractError::WrongPassword), true) => {}
                (other, _) => panic!("unexpected result: {other:?}"),
            }
            // 암호 없는 앞쪽 항목도 아직 풀지 않았어야 합니다.
            assert_eq!(progress.fraction(), Some(0.0));
        }
        assert_eq!(unpack(&archive, Some("pw")).unwrap().files, 2);
    }

    #[test]
    fn a_damaged_aes_zip_is_reported_as_damaged_not_as_a_wrong_password() {
        let work = Workspace::new("zip-aes-damaged");
        // 압축되지 않는 내용이어야 가운데 바이트가 데이터 영역에 놓입니다.
        let mut seed = 12345u32;
        let payload: Vec<u8> = (0..20_000)
            .map(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                (seed >> 16) as u8
            })
            .collect();
        let mut bytes = encrypted_zip(&[("data.bin", &payload)], true, b"pw");
        let middle = bytes.len() / 2;
        bytes[middle] ^= 0xFF;
        let archive = work.file("damaged.zip", &bytes);

        assert!(matches!(unpack(&archive, Some("bad")), Err(ExtractError::WrongPassword)));
        assert!(matches!(unpack(&archive, Some("pw")), Err(ExtractError::Failed(_))));
        assert_eq!(work.names(), vec!["damaged.zip"]);
    }

    #[test]
    fn calendar_dates_convert_to_system_time() {
        let at = |seconds| Some(UNIX_EPOCH + Duration::from_secs(seconds));
        assert_eq!(utc_to_system_time(1970, 1, 1, 0, 0, 0), at(0));
        assert_eq!(utc_to_system_time(2000, 2, 29, 12, 0, 0), at(951_825_600));
        assert_eq!(utc_to_system_time(2026, 11, 14, 9, 30, 15), at(1_794_648_615));
        assert_eq!(filetime_to_system_time(116_444_736_000_000_000), at(0));
        assert_eq!(filetime_to_system_time(0), None);
    }
}
