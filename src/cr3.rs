//! Canon CR3(RAW) 파일에서 카메라가 만들어 둔 JPEG를 꺼냅니다.
//!
//! CR3는 MP4와 같은 ISO-BMFF 컨테이너이며, 필요한 부분은 다음과 같습니다.
//!
//! - `moov/uuid(Canon)/CMT1`: TIFF IFD0. 내장 JPEG에는 EXIF가 없어 회전 정보는 여기서만 얻습니다.
//! - `moov/trak`: 보통 첫 트랙이 센서 크기의 JPEG이고, 나머지는 CRX RAW와 메타데이터입니다.
//! - `uuid(미리보기)/PRVW`: 1620x1080 JPEG. `mdat`보다 앞에 있어 뒤가 잘린 파일에서도 읽힙니다.
//!
//! RAW를 현상하지 않으므로 일반 JPEG만큼 빠르고 카메라의 픽처 스타일이 그대로 보입니다.
//! HDR PQ로 촬영한 파일은 미리보기가 모두 HEVC라서 표시하지 못합니다.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{bail, Context as _};
use image::metadata::Orientation;

const CANON_UUID: [u8; 16] = [
    0x85, 0xc0, 0xb6, 0x87, 0x82, 0x0f, 0x11, 0xe0, 0x81, 0x11, 0xf4, 0xce, 0x46, 0x2b, 0x6a, 0x48,
];
const PREVIEW_UUID: [u8; 16] = [
    0xea, 0xf4, 0x2b, 0x5e, 0x1c, 0x98, 0x4b, 0x88, 0xb9, 0xfb, 0xb7, 0xdc, 0x40, 0x6e, 0x4d, 0x16,
];
const JPEG_MAGIC: [u8; 3] = [0xff, 0xd8, 0xff];
const TIFF_TAG_ORIENTATION: u16 = 0x0112;

pub struct Cr3Preview {
    pub jpeg: Vec<u8>,
    /// `CMT1`에서 읽은 회전 정보. 찾지 못하면 `None`입니다.
    pub orientation: Option<Orientation>,
}

/// 확장자가 아니라 파일 앞부분의 `ftyp` 브랜드로 CR3인지 판별합니다.
pub fn is_cr3_file(path: &Path) -> bool {
    let mut head = [0u8; 12];
    File::open(path)
        .and_then(|mut file| file.read_exact(&mut head))
        .is_ok()
        && &head[4..12] == b"ftypcrx "
}

/// 가장 큰 내장 JPEG를 읽습니다. 트랙에 쓸 만한 JPEG가 없으면 `PRVW`로 대신합니다.
pub fn read_preview<R: Read + Seek>(reader: &mut R) -> anyhow::Result<Cr3Preview> {
    const READ_FAILED: &str = "CR3 파일을 읽지 못했습니다";

    let file_len = reader.seek(SeekFrom::End(0)).context(READ_FAILED)?;
    let top = scan_top_level(reader, file_len).context(READ_FAILED)?;
    let moov = top.moov.context("CR3 구조가 올바르지 않습니다 (moov 없음)")?;
    let moov = read_range(reader, moov).context(READ_FAILED)?;
    let orientation = read_orientation(&moov);

    if let Some(track) = largest_jpeg_track(reader, &moov, file_len).context(READ_FAILED)? {
        let jpeg = read_range(reader, track).context(READ_FAILED)?;
        return Ok(Cr3Preview { jpeg, orientation });
    }
    if let Some(preview) = top.preview {
        let body = read_range(reader, preview).context(READ_FAILED)?;
        if let Some(jpeg) = prvw_jpeg(&body) {
            return Ok(Cr3Preview { jpeg: jpeg.to_vec(), orientation });
        }
    }
    bail!("표시할 수 있는 JPEG 미리보기가 없는 CR3입니다 (HDR PQ 촬영 파일은 미지원)")
}

/// 파일 안의 바이트 구간.
#[derive(Clone, Copy)]
struct Range {
    offset: u64,
    len: u64,
}

#[derive(Default)]
struct TopLevel {
    moov: Option<Range>,
    preview: Option<Range>,
}

struct BoxHeader {
    kind: [u8; 4],
    /// 헤더 길이(8 또는 16바이트).
    len: usize,
    /// 헤더를 포함한 상자 크기. `None`이면 부모의 끝까지 이어집니다.
    size: Option<u64>,
}

impl BoxHeader {
    fn parse(bytes: &[u8]) -> Option<Self> {
        let size = be_u32(bytes, 0)?;
        let kind = bytes.get(4..8)?.try_into().ok()?;
        let (len, size) = match size {
            0 => (8, None),
            1 => (16, Some(be_u64(bytes, 8)?)),
            size => (8, Some(u64::from(size))),
        };
        // 헤더보다 작은 상자를 받아들이면 같은 자리를 끝없이 돌게 됩니다.
        match size {
            Some(size) if size < len as u64 => None,
            _ => Some(Self { kind, len, size }),
        }
    }
}

/// 메모리에 읽어 둔 컨테이너의 자식 상자를 차례로 돌려줍니다.
struct Boxes<'a>(&'a [u8]);

impl<'a> Iterator for Boxes<'a> {
    type Item = ([u8; 4], &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        let header = BoxHeader::parse(self.0)?;
        let end = match header.size {
            Some(size) => usize::try_from(size).ok()?,
            None => self.0.len(),
        };
        let body = self.0.get(header.len..end)?;
        self.0 = &self.0[end..];
        Some((header.kind, body))
    }
}

fn child<'a>(parent: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    Boxes(parent).find(|(k, _)| k == kind).map(|(_, body)| body)
}

/// 최상위 상자를 헤더만 읽으며 훑습니다. 수십 MB인 `mdat`은 건너뜁니다.
fn scan_top_level<R: Read + Seek>(reader: &mut R, file_len: u64) -> io::Result<TopLevel> {
    let mut top = TopLevel::default();
    let mut pos = 0u64;
    while pos < file_len {
        reader.seek(SeekFrom::Start(pos))?;
        // 가장 긴 헤더(16바이트) 뒤에 UUID(16바이트)가 올 수 있습니다.
        let mut head = Vec::with_capacity(32);
        reader.by_ref().take(32).read_to_end(&mut head)?;
        let Some(header) = BoxHeader::parse(&head) else {
            break;
        };

        let end = header.size.map_or(file_len, |size| pos.saturating_add(size));
        let offset = pos + header.len as u64;
        let body = Range { offset, len: end.min(file_len).saturating_sub(offset) };
        match &header.kind {
            b"moov" => top.moov = Some(body),
            b"uuid" if head[header.len..].starts_with(&PREVIEW_UUID) => top.preview = Some(body),
            _ => {}
        }
        pos = end;
    }
    Ok(top)
}

/// 호출하는 쪽에서 구간이 파일 안에 있음을 보장하므로 할당량은 파일 크기를 넘지 않습니다.
fn read_range<R: Read + Seek>(reader: &mut R, range: Range) -> io::Result<Vec<u8>> {
    reader.seek(SeekFrom::Start(range.offset))?;
    let mut bytes = vec![0; range.len as usize];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn largest_jpeg_track<R: Read + Seek>(
    reader: &mut R,
    moov: &[u8],
    file_len: u64,
) -> io::Result<Option<Range>> {
    let mut best: Option<Range> = None;
    for (_, trak) in Boxes(moov).filter(|(kind, _)| kind == b"trak") {
        let Some(sample) = first_sample(trak) else {
            continue;
        };
        // 뒤가 잘린 파일이면 이 구간이 파일 밖을 가리킵니다.
        let in_file = sample.offset.checked_add(sample.len).is_some_and(|end| end <= file_len);
        if !in_file || best.is_some_and(|b| sample.len <= b.len) {
            continue;
        }
        let head = Range { offset: sample.offset, len: sample.len.min(JPEG_MAGIC.len() as u64) };
        if read_range(reader, head)?.starts_with(&JPEG_MAGIC) {
            best = Some(sample);
        }
    }
    Ok(best)
}

/// 트랙의 첫 샘플 위치. 연속 촬영 파일은 샘플이 여럿이지만 첫 장만 보여 줍니다.
fn first_sample(trak: &[u8]) -> Option<Range> {
    let mdia = child(trak, b"mdia")?;
    let minf = child(mdia, b"minf")?;
    let stbl = child(minf, b"stbl")?;

    // stsz: 버전·플래그(4), 고정 크기(4), 샘플 수(4), 샘플별 크기(4씩)
    let stsz = child(stbl, b"stsz")?;
    if be_u32(stsz, 8)? == 0 {
        return None;
    }
    let len = match be_u32(stsz, 4)? {
        0 => be_u32(stsz, 12)?,
        fixed => fixed,
    };
    // co64: 버전·플래그(4), 항목 수(4), 오프셋(8씩)
    let offset = be_u64(child(stbl, b"co64")?, 8)?;
    Some(Range { offset, len: u64::from(len) })
}

fn read_orientation(moov: &[u8]) -> Option<Orientation> {
    Boxes(moov)
        .filter(|(kind, _)| kind == b"uuid")
        .find_map(|(_, body)| child(body.strip_prefix(&CANON_UUID)?, b"CMT1"))
        .and_then(tiff_orientation)
}

fn tiff_orientation(tiff: &[u8]) -> Option<Orientation> {
    let little_endian = match tiff.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |offset: usize| {
        let bytes = tiff.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
        Some(if little_endian { u16::from_le_bytes(bytes) } else { u16::from_be_bytes(bytes) })
    };
    let u32_at = |offset: usize| {
        let bytes = tiff.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
        Some(if little_endian { u32::from_le_bytes(bytes) } else { u32::from_be_bytes(bytes) })
    };

    let ifd = usize::try_from(u32_at(4)?).ok()?;
    let value = (0..usize::from(u16_at(ifd)?)).find_map(|index| {
        let entry = ifd.checked_add(2 + 12 * index)?;
        (u16_at(entry)? == TIFF_TAG_ORIENTATION).then(|| u16_at(entry.checked_add(8)?))?
    })?;
    Orientation::from_exif(u8::try_from(value).ok()?)
}

/// 미리보기 uuid 상자 본문: UUID(16)와 u64(8) 뒤에 `PRVW` 상자가 옵니다.
/// `PRVW` 본문은 16바이트 머리(마지막 4바이트가 JPEG 크기) 뒤에 JPEG가 이어집니다.
fn prvw_jpeg(uuid_body: &[u8]) -> Option<&[u8]> {
    let prvw = child(uuid_body.get(24..)?, b"PRVW")?;
    let len = usize::try_from(be_u32(prvw, 12)?).ok()?;
    let jpeg = prvw.get(16..len.checked_add(16)?)?;
    jpeg.starts_with(&JPEG_MAGIC).then_some(jpeg)
}

fn be_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(offset..offset + 4)?.try_into().ok()?))
}

fn be_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_be_bytes(bytes.get(offset..offset + 8)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }

    fn jpeg(width: u32, height: u32) -> Vec<u8> {
        let mut out = Vec::new();
        image::RgbImage::new(width, height)
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Jpeg)
            .unwrap();
        out
    }

    fn cmt1(orientation: u16) -> Vec<u8> {
        let mut tiff = b"II*\0".to_vec();
        tiff.extend_from_slice(&8u32.to_le_bytes());
        tiff.extend_from_slice(&1u16.to_le_bytes());
        tiff.extend_from_slice(&TIFF_TAG_ORIENTATION.to_le_bytes());
        tiff.extend_from_slice(&3u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&orientation.to_le_bytes());
        tiff.extend_from_slice(&[0, 0]);
        bx(b"CMT1", &tiff)
    }

    fn trak(offset: u64, len: usize) -> Vec<u8> {
        let stsz = [&[0u8; 8][..], &1u32.to_be_bytes(), &(len as u32).to_be_bytes()].concat();
        let co64 = [&[0u8; 4][..], &1u32.to_be_bytes(), &offset.to_be_bytes()].concat();
        let stbl = [bx(b"stsz", &stsz), bx(b"co64", &co64)].concat();
        bx(b"trak", &bx(b"mdia", &bx(b"minf", &bx(b"stbl", &stbl))))
    }

    /// 실제 CR3와 같은 순서(ftyp, moov, 미리보기 uuid, mdat)로 파일을 만듭니다.
    fn build_cr3(samples: &[&[u8]], orientation: u16, prvw: Option<&[u8]>) -> Vec<u8> {
        let ftyp = bx(b"ftyp", b"crx \0\0\0\x01crx isom");
        let preview = prvw.map_or_else(Vec::new, |payload| {
            let head = [&[0u8; 12][..], &(payload.len() as u32).to_be_bytes()].concat();
            let prvw = bx(b"PRVW", &[&head[..], payload].concat());
            bx(b"uuid", &[&PREVIEW_UUID[..], &[0; 8], &prvw].concat())
        });
        let moov = |mut offset: u64| {
            let canon = bx(b"uuid", &[&CANON_UUID[..], &cmt1(orientation)].concat());
            let mut body = canon;
            for sample in samples {
                body.extend(trak(offset, sample.len()));
                offset += sample.len() as u64;
            }
            bx(b"moov", &body)
        };
        // moov 크기는 오프셋 값과 무관하므로 먼저 재서 mdat 본문의 위치를 구합니다.
        let mdat_body = (ftyp.len() + moov(0).len() + preview.len() + 8) as u64;
        [ftyp, moov(mdat_body), preview, bx(b"mdat", &samples.concat())].concat()
    }

    fn read(file: &[u8]) -> anyhow::Result<Cr3Preview> {
        read_preview(&mut Cursor::new(file))
    }

    #[test]
    fn picks_largest_jpeg_track_and_skips_raw_tracks() {
        let (small, large) = (jpeg(8, 4), jpeg(64, 32));
        let mut crx = vec![0xaa; large.len() * 2];
        crx[..4].copy_from_slice(&[0xff, 0x01, 0x00, 0x08]);

        let preview = read(&build_cr3(&[&small, &large, &crx], 6, None)).unwrap();
        assert!(preview.jpeg == large);
        assert_eq!(preview.orientation, Some(Orientation::Rotate90));
    }

    #[test]
    fn falls_back_to_prvw_when_the_file_is_truncated() {
        let (full, small) = (jpeg(64, 32), jpeg(8, 4));
        let mut file = build_cr3(&[&full], 1, Some(&small));
        file.truncate(file.len() - 10);

        assert!(read(&file).unwrap().jpeg == small);
    }

    #[test]
    fn hevc_only_file_is_reported_as_unsupported() {
        let hevc = [0x00, 0x04, 0xe9, 0x60, 0x40, 0x01, 0x0c, 0x01];
        let file = build_cr3(&[&hevc], 1, Some(&hevc));

        let message = read(&file).err().unwrap().to_string();
        assert!(message.contains("HDR PQ"), "{message}");
    }

    #[test]
    fn damaged_files_never_panic() {
        let (full, small) = (jpeg(16, 8), jpeg(8, 4));
        let file = build_cr3(&[&full], 8, Some(&small));

        for len in 0..file.len() {
            let _ = read(&file[..len]);
        }
        for index in 0..file.len() {
            for value in [0x00, 0x01, 0xff] {
                let mut damaged = file.clone();
                damaged[index] = value;
                let _ = read(&damaged);
            }
        }
    }

    #[test]
    fn missing_orientation_is_none() {
        let file = build_cr3(&[&jpeg(8, 4)], 0, None);
        assert_eq!(read(&file).unwrap().orientation, None);
    }

    #[test]
    fn decode_detects_cr3_by_content_and_applies_rotation() {
        let path = std::env::temp_dir().join(format!("aspartame-cr3-test-{}.bin", std::process::id()));
        std::fs::write(&path, build_cr3(&[&jpeg(64, 32)], 8, None)).unwrap();

        let decoded = crate::image_loader::decode(&path, 4096);
        std::fs::remove_file(&path).unwrap();

        let decoded = decoded.unwrap();
        assert_eq!((decoded.width, decoded.height), (32, 64));
    }

    #[test]
    fn other_files_are_not_detected_as_cr3() {
        let path = std::env::temp_dir().join(format!("aspartame-not-cr3-test-{}.bin", std::process::id()));
        std::fs::write(&path, bx(b"ftyp", b"heic\0\0\0\0")).unwrap();

        let detected = is_cr3_file(&path);
        std::fs::remove_file(&path).unwrap();

        assert!(!detected);
    }
}
