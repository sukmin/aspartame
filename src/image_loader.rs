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

use crate::cr3;

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
        let result = decode(&request.path, request.max_side).map_err(|e| format!("{e:#}"));
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

/// Decodes `path`, applies the EXIF orientation and downsizes it so neither
/// side exceeds `max_side`.
pub fn decode(path: &Path, max_side: u32) -> anyhow::Result<DecodedImage> {
    let file_size = std::fs::metadata(path)
        .with_context(|| format!("cannot read file: {}", path.display()))?
        .len();

    let mut image = if cr3::is_cr3_file(path) {
        decode_cr3(path)?
    } else {
        decode_standard(path)?
    };

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
    decode_oriented(reader, None)
}

/// CR3 안의 JPEG를 디코딩합니다. 회전 정보는 JPEG가 아니라 컨테이너에 있습니다.
fn decode_cr3(path: &Path) -> anyhow::Result<DynamicImage> {
    let file = File::open(path)
        .with_context(|| format!("cannot open file: {}", path.display()))?;
    let preview = cr3::read_preview(&mut BufReader::new(file))?;
    let mut reader = ImageReader::with_format(Cursor::new(preview.jpeg), ImageFormat::Jpeg);
    reader.no_limits();
    decode_oriented(reader, preview.orientation)
}

/// `orientation`이 `None`이면 파일 자체의 EXIF 방향을 따릅니다.
fn decode_oriented<R: BufRead + Seek + 'static>(
    reader: ImageReader<R>,
    orientation: Option<Orientation>,
) -> anyhow::Result<DynamicImage> {
    let mut decoder = reader.into_decoder().context("unsupported or corrupt image")?;
    let orientation = match orientation {
        Some(orientation) => orientation,
        None => decoder.orientation().unwrap_or(Orientation::NoTransforms),
    };
    let mut image = DynamicImage::from_decoder(decoder).context("failed to decode image")?;
    image.apply_orientation(orientation);
    Ok(image)
}
