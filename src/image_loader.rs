//! Background image decoding.
//!
//! Requests are sent to a worker thread; results come back through a channel
//! and are polled once per frame. When the user presses the arrow keys faster
//! than images decode, queued requests are drained and only the newest
//! `Primary` request is decoded ("last request wins"). `Preload` requests are
//! only honoured while they belong to the current image.

use std::fs::File;
use std::io::{BufRead, BufReader, Cursor, Seek};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use anyhow::Context as _;
use image::imageops::FilterType;
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};

use crate::archive::{ArchiveReader, EntryRef};
use crate::cr3::{self, Cr3Preview};

/// zip 항목을 디코딩할 때 쓸 수 있는 메모리. 썸네일은 여러 스레드가 동시에 만들므로,
/// 몇 KB짜리 파일이 거대한 크기를 주장해도 프로세스가 중단되지 않게 막습니다.
const ZIP_ENTRY_MAX_ALLOC: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Priority {
    /// The image the user wants to see right now.
    Primary,
    /// A neighbour decoded ahead of time.
    Preload,
}

#[derive(Debug)]
pub struct LoadRequest {
    pub path: PathBuf,
    /// zip 안의 이미지이면 그 위치. 이때 `path`는 가상 경로입니다.
    pub entry: Option<EntryRef>,
    /// Increases with every navigation; stale results are discarded.
    pub generation: u64,
    pub priority: Priority,
    /// Images larger than this on either side are downscaled so the GPU can
    /// hold them as a single texture.
    pub max_side: u32,
}

pub struct DecodedImage {
    pub path: PathBuf,
    /// Size of the pixel buffer (after any downscaling).
    pub width: u32,
    pub height: u32,
    /// Size of the file's pixels (after EXIF orientation is applied).
    pub original_width: u32,
    pub original_height: u32,
    /// Straight (un-premultiplied) RGBA8, row-major.
    pub rgba: Vec<u8>,
    pub file_size: u64,
}

/// 갤러리 칸에 그릴 작은 이미지. 스트레이트 알파 RGBA8입니다.
pub struct ThumbnailImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub struct LoadResult {
    pub path: PathBuf,
    pub generation: u64,
    pub priority: Priority,
    pub result: Result<DecodedImage, String>,
}

pub struct ImageLoader {
    tx: Sender<LoadRequest>,
    rx: Receiver<LoadResult>,
}

impl ImageLoader {
    /// Spawns the worker thread. `ctx` is used to wake the UI when a result is ready.
    pub fn spawn(ctx: egui::Context) -> Self {
        let (req_tx, req_rx) = mpsc::channel::<LoadRequest>();
        let (res_tx, res_rx) = mpsc::channel::<LoadResult>();
        thread::Builder::new()
            .name("image-loader".into())
            .spawn(move || worker(req_rx, res_tx, ctx))
            .expect("failed to spawn image loader thread");
        Self { tx: req_tx, rx: res_rx }
    }

    pub fn request(&self, request: LoadRequest) {
        if self.tx.send(request).is_err() {
            log::error!("image loader thread is gone");
        }
    }

    pub fn try_recv(&self) -> Option<LoadResult> {
        self.rx.try_recv().ok()
    }
}

fn worker(rx: Receiver<LoadRequest>, tx: Sender<LoadResult>, ctx: egui::Context) {
    let mut queue: Vec<LoadRequest> = Vec::new();
    let mut latest_primary = 0u64;

    loop {
        if queue.is_empty() {
            match rx.recv() {
                Ok(request) => queue.push(request),
                Err(_) => return, // the app is gone
            }
        }
        while let Ok(request) = rx.try_recv() {
            queue.push(request);
        }

        for request in &queue {
            if request.priority == Priority::Primary {
                latest_primary = latest_primary.max(request.generation);
            }
        }
        // Drop everything the user has already navigated away from.
        queue.retain(|r| match r.priority {
            Priority::Primary => r.generation == latest_primary,
            Priority::Preload => r.generation >= latest_primary,
        });

        let pick = queue
            .iter()
            .position(|r| r.priority == Priority::Primary)
            .or_else(|| (!queue.is_empty()).then_some(0));
        let Some(pick) = pick else {
            continue;
        };
        let request = queue.remove(pick);
        // The same file may be queued twice (e.g. as preload and then primary).
        queue.retain(|r| r.path != request.path);

        log::debug!("decoding {} ({:?}, gen {})", request.path.display(), request.priority, request.generation);
        let started = std::time::Instant::now();
        let result = decode_request(&request).map_err(|e| format!("{e:#}"));
        log::debug!("decoded {} in {:?}", request.path.display(), started.elapsed());

        let sent = tx.send(LoadResult {
            path: request.path,
            generation: request.generation,
            priority: request.priority,
            result,
        });
        if sent.is_err() {
            return;
        }
        ctx.request_repaint();
    }
}

fn decode_request(request: &LoadRequest) -> anyhow::Result<DecodedImage> {
    let Some(entry) = &request.entry else {
        return decode(&request.path, request.max_side);
    };
    // 요청마다 새로 열어, 사용 중에 zip이 바뀌어도 옛 목록으로 읽지 않게 합니다.
    let bytes = ArchiveReader::open(&entry.archive)?.read_entry(entry.zip_index, entry.password.as_deref())?;
    let file_size = bytes.len() as u64;
    let image = decode_memory(bytes, &request.path, false)?;
    fit_to_texture(image, &request.path, file_size, request.max_side)
}

/// Decodes `path`, applies the EXIF orientation and downsizes it so neither
/// side exceeds `max_side`.
pub fn decode(path: &Path, max_side: u32) -> anyhow::Result<DecodedImage> {
    let file_size = std::fs::metadata(path)
        .with_context(|| format!("cannot read file: {}", path.display()))?
        .len();

    let image = if cr3::is_cr3_file(path) {
        decode_cr3(path)?
    } else {
        decode_standard(path)?
    };
    fit_to_texture(image, path, file_size, max_side)
}

/// zip에서 읽은 이미지로 썸네일을 만듭니다. `name`은 형식 판별의 힌트로만 씁니다.
pub fn decode_thumbnail(bytes: Vec<u8>, name: &Path, side: u32) -> anyhow::Result<ThumbnailImage> {
    let image = decode_memory(bytes, name, true)?;
    anyhow::ensure!(image.width() > 0 && image.height() > 0, "image has no pixels");
    // `thumbnail()`은 `resize()`보다 훨씬 빠른 평균 축소입니다.
    let rgba = if image.width().max(image.height()) > side {
        image.thumbnail(side, side).into_rgba8()
    } else {
        image.into_rgba8()
    };
    let (width, height) = rgba.dimensions();
    Ok(ThumbnailImage { width, height, rgba: rgba.into_raw() })
}

fn fit_to_texture(
    mut image: DynamicImage,
    path: &Path,
    file_size: u64,
    max_side: u32,
) -> anyhow::Result<DecodedImage> {
    let (original_width, original_height) = (image.width(), image.height());
    anyhow::ensure!(original_width > 0 && original_height > 0, "image has no pixels");

    let max_side = max_side.max(64);
    let longest = original_width.max(original_height);
    if longest > max_side {
        let ratio = f64::from(max_side) / f64::from(longest);
        let nw = ((f64::from(original_width) * ratio).round() as u32).max(1);
        let nh = ((f64::from(original_height) * ratio).round() as u32).max(1);
        log::info!(
            "downscaling {}x{} to {}x{} (GPU limit {})",
            original_width, original_height, nw, nh, max_side
        );
        image = image.resize(nw, nh, FilterType::Triangle);
    }

    let rgba = image.into_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(DecodedImage {
        path: path.to_path_buf(),
        width,
        height,
        original_width,
        original_height,
        rgba: rgba.into_raw(),
        file_size,
    })
}

fn decode_standard(path: &Path) -> anyhow::Result<DynamicImage> {
    let mut reader = ImageReader::open(path)
        .with_context(|| format!("cannot open file: {}", path.display()))?;
    // The default limits reject perfectly valid very large photos.
    reader.no_limits();
    let reader = reader
        .with_guessed_format()
        .context("cannot detect the image format")?;
    decode_oriented(reader, None, u64::MAX)
}

/// CR3 안의 JPEG를 디코딩합니다. 회전 정보는 JPEG가 아니라 컨테이너에 있습니다.
fn decode_cr3(path: &Path) -> anyhow::Result<DynamicImage> {
    let file = File::open(path)
        .with_context(|| format!("cannot open file: {}", path.display()))?;
    decode_cr3_preview(cr3::read_preview(&mut BufReader::new(file))?, u64::MAX)
}

fn decode_cr3_preview(preview: Cr3Preview, max_bytes: u64) -> anyhow::Result<DynamicImage> {
    let mut reader = ImageReader::with_format(Cursor::new(preview.jpeg), ImageFormat::Jpeg);
    reader.no_limits();
    decode_oriented(reader, preview.orientation, max_bytes)
}

/// zip에서 읽은 이미지를 디코딩합니다. `name`은 형식 판별의 힌트로만 씁니다.
/// `small_cr3`이면 CR3의 작은 내장 JPEG를 먼저 씁니다.
fn decode_memory(bytes: Vec<u8>, name: &Path, small_cr3: bool) -> anyhow::Result<DynamicImage> {
    if cr3::has_cr3_signature(&bytes) {
        let mut cursor = Cursor::new(bytes);
        let preview = if small_cr3 {
            cr3::read_small_preview(&mut cursor)?
        } else {
            cr3::read_preview(&mut cursor)?
        };
        return decode_cr3_preview(preview, ZIP_ENTRY_MAX_ALLOC);
    }
    let mut reader = ImageReader::new(Cursor::new(bytes));
    // TGA처럼 서명이 없는 형식은 확장자로만 알 수 있습니다.
    if let Ok(format) = ImageFormat::from_path(name) {
        reader.set_format(format);
    }
    reader.no_limits();
    let reader = reader
        .with_guessed_format()
        .context("cannot detect the image format")?;
    decode_oriented(reader, None, ZIP_ENTRY_MAX_ALLOC)
}

/// `orientation`이 `None`이면 파일 자체의 EXIF 방향을 따릅니다.
/// 픽셀 버퍼가 `max_bytes`를 넘는 이미지는 디코딩하지 않습니다.
fn decode_oriented<R: BufRead + Seek + 'static>(
    reader: ImageReader<R>,
    orientation: Option<Orientation>,
    max_bytes: u64,
) -> anyhow::Result<DynamicImage> {
    let mut decoder = reader.into_decoder().context("unsupported or corrupt image")?;
    // `image`의 메모리 상한은 `ImageReader::decode()`에서만 적용되므로 직접 검사합니다.
    anyhow::ensure!(decoder.total_bytes() <= max_bytes, "이미지가 너무 커서 표시할 수 없습니다");
    let orientation = match orientation {
        Some(orientation) => orientation,
        None => decoder.orientation().unwrap_or(Orientation::NoTransforms),
    };
    let mut image = DynamicImage::from_decoder(decoder).context("failed to decode image")?;
    image.apply_orientation(orientation);
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zip_entries_claiming_huge_dimensions_are_rejected() {
        // 54바이트 BMP 헤더가 60000x60000 크기(약 10GB)를 주장합니다.
        let mut bmp = b"BM".to_vec();
        bmp.extend_from_slice(&54u32.to_le_bytes());
        bmp.extend_from_slice(&[0; 4]);
        bmp.extend_from_slice(&54u32.to_le_bytes());
        bmp.extend_from_slice(&40u32.to_le_bytes());
        bmp.extend_from_slice(&60_000i32.to_le_bytes());
        bmp.extend_from_slice(&60_000i32.to_le_bytes());
        bmp.extend_from_slice(&1u16.to_le_bytes());
        bmp.extend_from_slice(&24u16.to_le_bytes());
        bmp.extend_from_slice(&[0; 24]);

        let error = decode_memory(bmp, Path::new("bomb.bmp"), true).err().unwrap();
        let message = format!("{error:#}");
        assert!(message.contains("너무 커서"), "{message}");
    }
}
