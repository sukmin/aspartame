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
const TIFF_TAG_EXIF_POINTER: u16 = 0x8769;
const TIFF_TAG_GPS_POINTER: u16 = 0x8825;
const GPS_TAG_LATITUDE: u16 = 0x0002;
/// 값이 다른 IFD의 오프셋이라 그대로 복사하면 엉뚱한 위치를 가리키는 태그.
const EXIF_TAG_INTEROP_POINTER: u16 = 0xA005;
/// `CMT1`에 있지만 JPEG에서는 SOF가 대신하는 태그(크기, 비트 수, 압축 방식).
const TIFF_TAGS_DESCRIBING_PIXELS: [u16; 4] = [0x0100, 0x0101, 0x0102, 0x0103];
/// APP1 세그먼트의 길이 필드는 16비트이고 자신의 2바이트와 `Exif\0\0` 6바이트를 포함합니다.
const MAX_EXIF_TIFF_LEN: usize = u16::MAX as usize - 8;

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
        && has_cr3_signature(&head)
}

pub fn has_cr3_signature(head: &[u8]) -> bool {
    head.get(4..12) == Some(b"ftypcrx ")
}

pub fn has_cr3_extension(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("cr3"))
}

/// 내장 JPEG를 찾아볼 위치.
#[derive(Clone, Copy)]
enum PreviewSource {
    /// 트랙에 든 가장 큰 JPEG. 보통 센서 크기입니다.
    Track,
    /// 1620x1080 `PRVW`.
    Prvw,
}

/// 표시용 JPEG와 회전 정보를 읽습니다.
pub fn read_preview<R: Read + Seek>(reader: &mut R) -> anyhow::Result<Cr3Preview> {
    let (moov, jpeg) = read_moov_and_jpeg(reader, [PreviewSource::Track, PreviewSource::Prvw])?;
    Ok(Cr3Preview { jpeg, orientation: read_orientation(&moov) })
}

/// 썸네일용으로 작은 `PRVW`를 먼저 읽습니다. 센서 크기 JPEG보다 디코딩이 훨씬 빠릅니다.
pub fn read_small_preview<R: Read + Seek>(reader: &mut R) -> anyhow::Result<Cr3Preview> {
    let (moov, jpeg) = read_moov_and_jpeg(reader, [PreviewSource::Prvw, PreviewSource::Track])?;
    Ok(Cr3Preview { jpeg, orientation: read_orientation(&moov) })
}

/// 저장용 JPEG를 만듭니다. 내장 JPEG에 없는 촬영·회전 정보를 EXIF로 넣으며,
/// 이미지 데이터는 다시 압축하지 않으므로 화질이 변하지 않습니다.
pub fn export_jpeg<R: Read + Seek>(reader: &mut R) -> anyhow::Result<Vec<u8>> {
    let (moov, jpeg) = read_moov_and_jpeg(reader, [PreviewSource::Track, PreviewSource::Prvw])?;
    Ok(match build_exif(&moov) {
        Some(tiff) => insert_exif(&jpeg, &tiff),
        None => jpeg,
    })
}

/// `moov`와 내장 JPEG를 읽습니다. `order` 순서로 찾아 처음 나온 JPEG를 씁니다.
fn read_moov_and_jpeg<R: Read + Seek>(
    reader: &mut R,
    order: [PreviewSource; 2],
) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    const READ_FAILED: &str = "CR3 파일을 읽지 못했습니다";

    let file_len = reader.seek(SeekFrom::End(0)).context(READ_FAILED)?;
    let top = scan_top_level(reader, file_len).context(READ_FAILED)?;
    let moov = top.moov.context("CR3 구조가 올바르지 않습니다 (moov 없음)")?;
    let moov = read_range(reader, moov).context(READ_FAILED)?;

    for source in order {
        let jpeg = match source {
            PreviewSource::Track => read_track_jpeg(reader, &moov, file_len),
            PreviewSource::Prvw => read_prvw_jpeg(reader, top.preview),
        };
        if let Some(jpeg) = jpeg.context(READ_FAILED)? {
            return Ok((moov, jpeg));
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

fn read_track_jpeg<R: Read + Seek>(
    reader: &mut R,
    moov: &[u8],
    file_len: u64,
) -> io::Result<Option<Vec<u8>>> {
    match largest_jpeg_track(reader, moov, file_len)? {
        Some(track) => read_range(reader, track).map(Some),
        None => Ok(None),
    }
}

fn read_prvw_jpeg<R: Read + Seek>(
    reader: &mut R,
    preview: Option<Range>,
) -> io::Result<Option<Vec<u8>>> {
    let Some(preview) = preview else {
        return Ok(None);
    };
    let body = read_range(reader, preview)?;
    Ok(prvw_jpeg(&body).map(<[u8]>::to_vec))
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

fn canon_metadata(moov: &[u8]) -> Option<&[u8]> {
    Boxes(moov)
        .filter(|(kind, _)| kind == b"uuid")
        .find_map(|(_, body)| body.strip_prefix(&CANON_UUID))
}

fn read_orientation(moov: &[u8]) -> Option<Orientation> {
    child(canon_metadata(moov)?, b"CMT1").and_then(tiff_orientation)
}

/// TIFF IFD의 항목 하나. `value`는 리틀엔디언 원본 바이트입니다.
struct IfdEntry {
    tag: u16,
    kind: u16,
    count: u32,
    value: Vec<u8>,
}

impl IfdEntry {
    fn long(tag: u16, value: u32) -> Self {
        Self { tag, kind: 4, count: 1, value: value.to_le_bytes().to_vec() }
    }

    /// IFD 뒤쪽 데이터 영역에서 차지하는 크기. 4바이트 이하는 항목 안에 들어갑니다.
    /// TIFF는 오프셋을 짝수로 맞춰야 합니다.
    fn data_len(&self) -> usize {
        match self.value.len() {
            0..=4 => 0,
            len => len + len % 2,
        }
    }
}

fn tiff_type_size(kind: u16) -> Option<usize> {
    match kind {
        1 | 2 | 6 | 7 => Some(1),
        3 | 8 => Some(2),
        4 | 9 | 11 => Some(4),
        5 | 10 | 12 => Some(8),
        _ => None,
    }
}

/// 리틀엔디언 TIFF의 첫 IFD를 읽습니다. 지금까지 확인한 CR3는 모두 리틀엔디언입니다.
fn read_ifd(tiff: &[u8]) -> Option<Vec<IfdEntry>> {
    if !tiff.starts_with(b"II*\0") {
        return None;
    }
    let u16_at = |offset: usize| {
        Some(u16::from_le_bytes(tiff.get(offset..offset.checked_add(2)?)?.try_into().ok()?))
    };
    let u32_at = |offset: usize| {
        Some(u32::from_le_bytes(tiff.get(offset..offset.checked_add(4)?)?.try_into().ok()?))
    };

    let ifd = usize::try_from(u32_at(4)?).ok()?;
    let mut entries = Vec::new();
    for index in 0..usize::from(u16_at(ifd)?) {
        let entry = ifd.checked_add(2 + 12 * index)?;
        let (tag, kind, count) = (u16_at(entry)?, u16_at(entry + 2)?, u32_at(entry + 4)?);
        let Some(size) = tiff_type_size(kind) else {
            continue;
        };
        let len = usize::try_from(count).ok()?.checked_mul(size)?;
        let start = match len {
            0..=4 => entry + 8,
            _ => usize::try_from(u32_at(entry + 8)?).ok()?,
        };
        let value = tiff.get(start..start.checked_add(len)?)?.to_vec();
        entries.push(IfdEntry { tag, kind, count, value });
    }
    Some(entries)
}

/// 헤더를 뺀 IFD의 전체 크기.
fn ifd_len(entries: &[IfdEntry]) -> usize {
    2 + 12 * entries.len() + 4 + entries.iter().map(IfdEntry::data_len).sum::<usize>()
}

/// `out`의 끝에 IFD를 씁니다. `out`의 시작이 TIFF 헤더여야 오프셋이 맞습니다.
fn write_ifd(out: &mut Vec<u8>, entries: &[IfdEntry]) {
    let mut data_offset = out.len() + 2 + 12 * entries.len() + 4;
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for entry in entries {
        out.extend_from_slice(&entry.tag.to_le_bytes());
        out.extend_from_slice(&entry.kind.to_le_bytes());
        out.extend_from_slice(&entry.count.to_le_bytes());
        if entry.data_len() == 0 {
            let mut inline = [0u8; 4];
            inline[..entry.value.len()].copy_from_slice(&entry.value);
            out.extend_from_slice(&inline);
        } else {
            out.extend_from_slice(&(data_offset as u32).to_le_bytes());
            data_offset += entry.data_len();
        }
    }
    out.extend_from_slice(&0u32.to_le_bytes()); // 다음 IFD 없음
    for entry in entries.iter().filter(|entry| entry.data_len() > 0) {
        out.extend_from_slice(&entry.value);
        out.resize(out.len() + entry.data_len() - entry.value.len(), 0);
    }
}

/// `CMT1`(IFD0), `CMT2`(Exif), `CMT4`(GPS)를 하나의 TIFF로 합칩니다.
/// 각 상자는 자기 시작점 기준의 오프셋을 쓰므로 그대로 이어 붙일 수 없습니다.
fn build_exif(moov: &[u8]) -> Option<Vec<u8>> {
    let canon = canon_metadata(moov)?;
    let mut ifd0 = read_ifd(child(canon, b"CMT1")?)?;
    let exif = child(canon, b"CMT2")
        .and_then(read_ifd)
        .map(|ifd| {
            let mut ifd = ifd;
            ifd.retain(|entry| entry.tag != EXIF_TAG_INTEROP_POINTER);
            ifd
        })
        .filter(|ifd| !ifd.is_empty());
    // GPS를 끈 사진의 CMT4에는 버전 태그만 있습니다.
    let gps = child(canon, b"CMT4")
        .and_then(read_ifd)
        .filter(|ifd| ifd.iter().any(|entry| entry.tag == GPS_TAG_LATITUDE));

    ifd0.retain(|entry| {
        !TIFF_TAGS_DESCRIBING_PIXELS.contains(&entry.tag)
            && entry.tag != TIFF_TAG_EXIF_POINTER
            && entry.tag != TIFF_TAG_GPS_POINTER
    });
    // 포인터 항목의 개수가 IFD0 크기를 정하므로 먼저 자리를 잡고 값은 나중에 씁니다.
    if exif.is_some() {
        ifd0.push(IfdEntry::long(TIFF_TAG_EXIF_POINTER, 0));
    }
    if gps.is_some() {
        ifd0.push(IfdEntry::long(TIFF_TAG_GPS_POINTER, 0));
    }
    ifd0.sort_by_key(|entry| entry.tag);

    let exif_offset = 8 + ifd_len(&ifd0);
    let gps_offset = exif_offset + exif.as_deref().map_or(0, ifd_len);
    for entry in &mut ifd0 {
        match entry.tag {
            TIFF_TAG_EXIF_POINTER => entry.value = (exif_offset as u32).to_le_bytes().to_vec(),
            TIFF_TAG_GPS_POINTER => entry.value = (gps_offset as u32).to_le_bytes().to_vec(),
            _ => {}
        }
    }

    let mut tiff = b"II*\0\x08\0\0\0".to_vec();
    write_ifd(&mut tiff, &ifd0);
    if let Some(exif) = &exif {
        write_ifd(&mut tiff, exif);
    }
    if let Some(gps) = &gps {
        write_ifd(&mut tiff, gps);
    }
    (tiff.len() <= MAX_EXIF_TIFF_LEN).then_some(tiff)
}

/// SOI 바로 뒤에 APP1(Exif) 세그먼트를 넣습니다.
fn insert_exif(jpeg: &[u8], tiff: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(jpeg.len() + tiff.len() + 10);
    out.extend_from_slice(&jpeg[..2]);
    out.extend_from_slice(&[0xff, 0xe1]);
    out.extend_from_slice(&((tiff.len() + 8) as u16).to_be_bytes());
    out.extend_from_slice(b"Exif\0\0");
    out.extend_from_slice(tiff);
    out.extend_from_slice(&jpeg[2..]);
    out
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
    fn small_preview_prefers_prvw_and_falls_back_to_the_track() {
        let (full, small) = (jpeg(64, 32), jpeg(8, 4));
        let with_prvw = build_cr3(&[&full], 6, Some(&small));
        let preview = read_small_preview(&mut Cursor::new(&with_prvw)).unwrap();
        assert!(preview.jpeg == small);
        assert_eq!(preview.orientation, Some(Orientation::Rotate90));

        let without_prvw = build_cr3(&[&full], 1, None);
        assert!(read_small_preview(&mut Cursor::new(&without_prvw)).unwrap().jpeg == full);
    }

    #[test]
    fn export_drops_pointer_tags_that_would_dangle() {
        let cmt = [cmt1(1), bx(b"CMT2", &tiff_with(EXIF_TAG_INTEROP_POINTER, 4, 1, &[0x40, 0, 0, 0]))];
        let file = build_cr3_with_metadata(&jpeg(8, 4), &cmt);
        let saved = export_jpeg(&mut Cursor::new(&file)).unwrap();
        let ifd0 = read_ifd(exif_of(&saved)).unwrap();
        // 남는 태그가 없으므로 Exif IFD 자체가 만들어지지 않아야 합니다.
        assert!(ifd0.iter().all(|entry| entry.tag != TIFF_TAG_EXIF_POINTER));
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

    /// 태그 하나짜리 TIFF. 값이 4바이트를 넘으면 뒤쪽 데이터 영역에 둡니다.
    fn tiff_with(tag: u16, kind: u16, count: u32, value: &[u8]) -> Vec<u8> {
        let mut tiff = b"II*\0".to_vec();
        tiff.extend_from_slice(&8u32.to_le_bytes());
        tiff.extend_from_slice(&1u16.to_le_bytes());
        tiff.extend_from_slice(&tag.to_le_bytes());
        tiff.extend_from_slice(&kind.to_le_bytes());
        tiff.extend_from_slice(&count.to_le_bytes());
        if value.len() <= 4 {
            let mut inline = [0u8; 4];
            inline[..value.len()].copy_from_slice(value);
            tiff.extend_from_slice(&inline);
            tiff.extend_from_slice(&[0; 4]);
        } else {
            tiff.extend_from_slice(&26u32.to_le_bytes());
            tiff.extend_from_slice(&[0; 4]);
            tiff.extend_from_slice(value);
        }
        tiff
    }

    /// 저장한 JPEG의 APP1에서 TIFF 부분을 꺼냅니다.
    fn exif_of(jpeg: &[u8]) -> &[u8] {
        assert_eq!(&jpeg[..4], &[0xff, 0xd8, 0xff, 0xe1]);
        let len = usize::from(u16::from_be_bytes([jpeg[4], jpeg[5]]));
        assert_eq!(&jpeg[6..12], b"Exif\0\0");
        &jpeg[12..4 + len]
    }

    /// 지정한 위치의 IFD를 읽을 수 있게 그 위치를 첫 IFD로 삼는 사본을 돌려줍니다.
    fn ifd_at(tiff: &[u8], offset: u32) -> Vec<IfdEntry> {
        let mut copy = tiff.to_vec();
        copy[4..8].copy_from_slice(&offset.to_le_bytes());
        read_ifd(&copy).unwrap()
    }

    fn value_of(entries: &[IfdEntry], tag: u16) -> Vec<u8> {
        entries.iter().find(|entry| entry.tag == tag).unwrap().value.clone()
    }

    fn pointer(entries: &[IfdEntry], tag: u16) -> u32 {
        u32::from_le_bytes(value_of(entries, tag).try_into().unwrap())
    }

    /// `moov/uuid(Canon)` 아래에 상자들을 넣은 파일을 만듭니다.
    fn build_cr3_with_metadata(jpeg: &[u8], metadata: &[Vec<u8>]) -> Vec<u8> {
        let ftyp = bx(b"ftyp", b"crx \0\0\0\x01crx isom");
        let moov = |offset: u64| {
            let canon = bx(b"uuid", &[&CANON_UUID[..], &metadata.concat()].concat());
            bx(b"moov", &[canon, trak(offset, jpeg.len())].concat())
        };
        let offset = (ftyp.len() + moov(0).len() + 8) as u64;
        [ftyp, moov(offset), bx(b"mdat", jpeg)].concat()
    }

    #[test]
    fn export_merges_metadata_and_keeps_image_data_unchanged() {
        let source = jpeg(16, 8);
        let model = b"Canon PowerShot V1\0";
        let date = b"2026:11:09 15:36:59\0";
        let cmt1 = [
            cmt1(6),
            bx(b"CMT2", &tiff_with(0x9003, 2, date.len() as u32, date)),
            bx(b"CMT4", &tiff_with(GPS_TAG_LATITUDE, 2, 2, b"N\0")),
        ];
        // CMT1은 값이 긴 태그도 다뤄야 하므로 모델명 태그를 함께 둔 파일도 확인합니다.
        let file = build_cr3_with_metadata(&source, &cmt1);
        let saved = export_jpeg(&mut Cursor::new(&file)).unwrap();

        let tiff = exif_of(&saved);
        let ifd0 = read_ifd(tiff).unwrap();
        assert_eq!(value_of(&ifd0, TIFF_TAG_ORIENTATION), 6u16.to_le_bytes());
        let tags: Vec<u16> = ifd0.iter().map(|entry| entry.tag).collect();
        assert!(tags.windows(2).all(|pair| pair[0] < pair[1]), "{tags:?}");

        let exif = ifd_at(tiff, pointer(&ifd0, TIFF_TAG_EXIF_POINTER));
        assert_eq!(value_of(&exif, 0x9003), date);
        let gps = ifd_at(tiff, pointer(&ifd0, TIFF_TAG_GPS_POINTER));
        assert_eq!(value_of(&gps, GPS_TAG_LATITUDE), b"N\0");

        // 이미지 데이터는 한 바이트도 바뀌지 않아야 합니다.
        assert!(saved.ends_with(&source[2..]));

        // 일반 EXIF 판독기도 같은 방향을 읽어야 합니다.
        assert_eq!(Orientation::from_exif_chunk(tiff), Some(Orientation::Rotate90));

        let long = bx(b"CMT1", &tiff_with(0x0110, 2, model.len() as u32, model));
        let file = build_cr3_with_metadata(&source, &[long]);
        let saved = export_jpeg(&mut Cursor::new(&file)).unwrap();
        assert_eq!(value_of(&read_ifd(exif_of(&saved)).unwrap(), 0x0110), model);
    }

    #[test]
    fn export_omits_gps_when_the_camera_recorded_none() {
        let file = build_cr3_with_metadata(
            &jpeg(8, 4),
            &[cmt1(1), bx(b"CMT4", &tiff_with(0x0000, 1, 4, &[2, 3, 0, 0]))],
        );
        let saved = export_jpeg(&mut Cursor::new(&file)).unwrap();
        let ifd0 = read_ifd(exif_of(&saved)).unwrap();
        assert!(ifd0.iter().all(|entry| entry.tag != TIFF_TAG_GPS_POINTER));
    }

    #[test]
    fn export_without_metadata_still_saves_the_jpeg() {
        let source = jpeg(8, 4);
        let file = build_cr3_with_metadata(&source, &[]);
        assert!(export_jpeg(&mut Cursor::new(&file)).unwrap() == source);
    }

    #[test]
    fn export_of_damaged_files_never_panics() {
        let cmt = [cmt1(8), bx(b"CMT2", &tiff_with(0x9003, 2, 6, b"abcde\0"))];
        let file = build_cr3_with_metadata(&jpeg(16, 8), &cmt);
        for index in 0..file.len() {
            for value in [0x00, 0x01, 0xff] {
                let mut damaged = file.clone();
                damaged[index] = value;
                let _ = export_jpeg(&mut Cursor::new(&damaged));
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
