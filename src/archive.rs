//! zip 파일 안의 이미지 목록을 만들고 내용을 읽습니다.
//!
//! zip 안의 이미지는 `앨범.zip\폴더\사진.jpg` 형태의 가상 경로로 구분합니다. 디스크에 실제로
//! 있는 경로는 아니지만, 덕분에 이전/다음 이동과 텍스처 캐시, 제목 표시를 그대로 쓸 수 있습니다.

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context as _};
use zip::ZipArchive;

use crate::navigator::{is_supported, natural_cmp};

pub const ZIP_EXTENSION: &str = "zip";

/// 손상되었거나 악의적인 zip이 메모리를 모두 쓰지 못하게 막는 항목당 상한.
const MAX_ENTRY_BYTES: u64 = 1024 * 1024 * 1024;
/// zip 헤더의 크기 값은 믿을 수 없으므로 처음 잡는 버퍼는 이만큼으로 제한합니다.
const MAX_INITIAL_CAPACITY: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchiveEntry {
    /// zip 안에서의 순번.
    pub zip_index: usize,
    /// 캐시 키이자 화면 표시용 가상 경로.
    pub path: PathBuf,
}

/// 이미지 로더에 넘기는 zip 항목의 위치.
#[derive(Clone, Debug)]
pub struct EntryRef {
    pub archive: PathBuf,
    pub zip_index: usize,
}

/// 이름순으로 정렬한 zip 안의 이미지 목록.
#[derive(Debug)]
pub struct Archive {
    pub path: PathBuf,
    pub entries: Vec<ArchiveEntry>,
}

impl Archive {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let zip = open_zip(&path)?;

        let mut named: Vec<(String, usize)> = zip
            .file_names_raw()
            .enumerate()
            .map(|(zip_index, raw)| (decode_name(raw), zip_index))
            .filter(|(name, _)| is_listed(name))
            .collect();
        named.sort_by(|a, b| natural_cmp(&a.0, &b.0));

        let mut entries: Vec<ArchiveEntry> = named
            .into_iter()
            .map(|(name, zip_index)| ArchiveEntry {
                zip_index,
                path: virtual_path(&path, &name),
            })
            .collect();
        // 가상 경로는 캐시 키이므로 겹치면 안 됩니다. 원본 이름으로 정렬했기 때문에
        // 같은 경로가 이웃해 있다는 보장은 없습니다.
        let mut seen = HashSet::new();
        entries.retain(|entry| seen.insert(entry.path.clone()));
        Ok(Self { path, entries })
    }

    pub fn entry_ref(&self, path: &Path) -> Option<EntryRef> {
        self.entries.iter().find(|entry| entry.path == path).map(|entry| EntryRef {
            archive: self.path.clone(),
            zip_index: entry.zip_index,
        })
    }

    pub fn paths(&self) -> Vec<PathBuf> {
        self.entries.iter().map(|entry| entry.path.clone()).collect()
    }
}

/// 열어 둔 zip에서 항목을 읽습니다. 스레드마다 하나씩 가집니다.
pub struct ArchiveReader {
    zip: ZipArchive<BufReader<File>>,
}

impl ArchiveReader {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        Ok(Self { zip: open_zip(path)? })
    }

    pub fn read_entry(&mut self, zip_index: usize) -> anyhow::Result<Vec<u8>> {
        let entry = self.zip.by_index(zip_index).context("zip 항목을 열지 못했습니다")?;
        let mut bytes = Vec::with_capacity(entry.size().min(MAX_INITIAL_CAPACITY) as usize);
        entry
            .take(MAX_ENTRY_BYTES + 1)
            .read_to_end(&mut bytes)
            .context("zip 항목의 압축을 풀지 못했습니다")?;
        ensure!(bytes.len() as u64 <= MAX_ENTRY_BYTES, "zip 항목이 너무 큽니다");
        Ok(bytes)
    }
}

/// 확장자가 아니라 파일 앞부분의 서명으로 zip인지 판별합니다.
pub fn is_zip_file(path: &Path) -> bool {
    let mut head = [0u8; 4];
    File::open(path)
        .and_then(|mut file| file.read_exact(&mut head))
        .is_ok()
        && (head == *b"PK\x03\x04" || head == *b"PK\x05\x06")
}

fn open_zip(path: &Path) -> anyhow::Result<ZipArchive<BufReader<File>>> {
    let file = File::open(path).with_context(|| format!("cannot open file: {}", path.display()))?;
    ZipArchive::new(BufReader::new(file)).context("zip 파일을 읽지 못했습니다")
}

/// UTF-8이 아닌 이름은 Windows 탐색기로 만든 옛 zip의 CP949로 해석합니다.
fn decode_name(raw: &[u8]) -> String {
    let name = match std::str::from_utf8(raw) {
        Ok(name) => name.to_owned(),
        Err(_) => encoding_rs::EUC_KR.decode(raw).0.into_owned(),
    };
    name.replace('\\', "/")
}

/// macOS가 넣는 `__MACOSX/` 폴더와 `._이름` 파일은 이미지가 아닙니다.
fn is_listed(name: &str) -> bool {
    let base = name.rsplit('/').next().unwrap_or(name);
    is_supported(Path::new(base)) && !base.starts_with("._") && !name.starts_with("__MACOSX/")
}

/// 절대 경로나 `..`가 섞인 이름도 항상 zip 경로 아래에 머물게 합니다.
fn virtual_path(archive: &Path, name: &str) -> PathBuf {
    let mut path = archive.to_path_buf();
    for part in name.split('/').filter(|part| !matches!(*part, "" | "." | "..")) {
        // `C:`처럼 콜론이 있으면 Windows에서 zip 바깥 경로로 해석됩니다.
        path.push(part.replace(':', "_"));
    }
    path
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::io::{Cursor, Write};

    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    /// 이름과 내용 목록으로 deflate 압축된 zip 바이트열을 만듭니다.
    pub fn build_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (name, bytes) in files {
            writer.start_file(*name, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    pub fn temp_file(label: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir()
            .join(format!("aspartame-{label}-{}.zip", std::process::id()));
        std::fs::write(&path, bytes).unwrap();
        path
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{build_zip, temp_file};
    use super::*;

    fn names(archive: &Archive) -> Vec<String> {
        archive
            .entries
            .iter()
            .map(|entry| entry.path.strip_prefix(&archive.path).unwrap().display().to_string())
            .map(|name| name.replace('\\', "/"))
            .collect()
    }

    #[test]
    fn lists_images_in_natural_order_and_skips_other_files() {
        let path = temp_file(
            "list",
            &build_zip(&[
                ("img10.jpg", b"a"),
                ("img2.PNG", b"b"),
                ("note.txt", b"c"),
                ("sub/photo.cr3", b"d"),
                ("__MACOSX/._img2.PNG", b"e"),
                ("._hidden.jpg", b"f"),
            ]),
        );
        let archive = Archive::open(&path);
        std::fs::remove_file(&path).unwrap();

        assert_eq!(
            names(&archive.unwrap()),
            vec!["img2.PNG", "img10.jpg", "sub/photo.cr3"]
        );
    }

    #[test]
    fn entries_with_the_same_virtual_path_appear_once() {
        // 정렬하면 "a0.jpg"가 두 "a.jpg" 경로 사이에 오지 않더라도 이웃 여부에 의존하면 안 됩니다.
        let path = temp_file(
            "dedup",
            &build_zip(&[("../a.jpg", b"x"), ("a0.jpg", b"y"), ("a.jpg", b"z"), ("b:c.jpg", b"1"), ("b_c.jpg", b"2")]),
        );
        let archive = Archive::open(&path);
        std::fs::remove_file(&path).unwrap();

        let names = names(&archive.unwrap());
        assert_eq!(names.iter().filter(|name| *name == "a.jpg").count(), 1, "{names:?}");
        assert_eq!(names.iter().filter(|name| *name == "b_c.jpg").count(), 1, "{names:?}");
        assert_eq!(names.len(), 3, "{names:?}");
    }

    #[test]
    fn reads_entry_bytes_through_entry_ref() {
        let path = temp_file("read", &build_zip(&[("a.jpg", b"first"), ("b.jpg", b"second")]));
        let archive = Archive::open(&path).unwrap();
        let entry = archive.entry_ref(&archive.entries[1].path).unwrap();
        let bytes = ArchiveReader::open(&entry.archive).unwrap().read_entry(entry.zip_index);
        std::fs::remove_file(&path).unwrap();

        assert_eq!(bytes.unwrap(), b"second");
    }

    #[test]
    fn decodes_cp949_names_from_old_windows_zips() {
        // "aaaa.jpg"를 넣은 뒤 이름 바이트만 CP949 "사진"으로 바꿉니다. UTF-8 플래그는 없습니다.
        let mut bytes = build_zip(&[("aaaa.jpg", b"x")]);
        let mut replaced = 0;
        for start in 0..bytes.len() - 4 {
            if &bytes[start..start + 4] == b"aaaa" {
                bytes[start..start + 4].copy_from_slice(&[0xbb, 0xe7, 0xc1, 0xf8]);
                replaced += 1;
            }
        }
        assert_eq!(replaced, 2);

        let path = temp_file("cp949", &bytes);
        let archive = Archive::open(&path);
        std::fs::remove_file(&path).unwrap();

        assert_eq!(names(&archive.unwrap()), vec!["사진.jpg"]);
    }

    #[test]
    fn virtual_paths_stay_inside_the_archive() {
        let archive = Path::new("C:\\photos\\album.zip");
        for name in ["../../evil.jpg", "/etc/evil.jpg", "C:/Windows/evil.jpg", "a/./../evil.jpg"] {
            let path = virtual_path(archive, name);
            assert!(path.starts_with(archive), "{name} -> {}", path.display());
            assert!(path.ends_with("evil.jpg"), "{name} -> {}", path.display());
        }
    }

    #[test]
    fn detects_zip_by_content_and_rejects_broken_zip() {
        let zip = temp_file("detect", &build_zip(&[("a.jpg", b"x")]));
        let broken = temp_file("broken", b"PK\x03\x04 not really a zip");
        let text = temp_file("text", b"hello world");

        let results = (is_zip_file(&zip), is_zip_file(&text), Archive::open(&broken).is_err());
        for path in [zip, broken, text] {
            std::fs::remove_file(path).unwrap();
        }
        assert_eq!(results, (true, false, true));
    }
}
