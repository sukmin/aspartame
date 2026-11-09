//! 갤러리 썸네일을 백그라운드에서 만드는 작업 스레드 묶음.
//!
//! 화면이 스크롤되면 대기열을 통째로 바꾸므로, 지나간 칸의 이미지는 디코딩하지 않습니다.

use std::collections::{HashSet, VecDeque};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use anyhow::Context as _;

use crate::archive::{Archive, ArchiveReader};
use crate::image_loader::{decode_thumbnail, ThumbnailImage};

const MAX_THREADS: usize = 4;

pub struct ThumbnailResult {
    /// `Archive::entries`에서의 순번.
    pub index: usize,
    pub result: Result<ThumbnailImage, String>,
}

#[derive(Default)]
struct Queue {
    waiting: VecDeque<usize>,
    running: HashSet<usize>,
    closed: bool,
}

#[derive(Default)]
struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
}

pub struct ThumbnailPool {
    shared: Arc<Shared>,
    rx: Receiver<ThumbnailResult>,
}

impl ThumbnailPool {
    pub fn spawn(ctx: egui::Context, archive: Arc<Archive>, side: u32) -> Self {
        let shared = Arc::new(Shared::default());
        let (tx, rx) = mpsc::channel();
        let threads = thread::available_parallelism().map_or(2, |n| n.get().clamp(2, MAX_THREADS));
        for number in 0..threads {
            let (shared, tx, ctx, archive) = (shared.clone(), tx.clone(), ctx.clone(), archive.clone());
            thread::Builder::new()
                .name(format!("thumbnail-{number}"))
                .spawn(move || worker(shared, tx, ctx, archive, side))
                .expect("failed to spawn thumbnail thread");
        }
        Self { shared, rx }
    }

    /// 대기열을 `wanted`(먼저 만들 순서)로 바꿉니다. 이미 만드는 중인 항목은 건너뜁니다.
    pub fn set_wanted(&self, wanted: &[usize]) {
        let mut queue = self.shared.queue.lock().unwrap();
        queue.waiting = wanted
            .iter()
            .copied()
            .filter(|index| !queue.running.contains(index))
            .collect();
        drop(queue);
        self.shared.wake.notify_all();
    }

    pub fn try_recv(&self) -> Option<ThumbnailResult> {
        let result = self.rx.try_recv().ok()?;
        // UI가 받을 때까지 `running`에 남겨, 채널에서 기다리는 결과를 다시 만들지 않습니다.
        self.shared.queue.lock().unwrap().running.remove(&result.index);
        Some(result)
    }
}

impl Drop for ThumbnailPool {
    fn drop(&mut self) {
        self.shared.queue.lock().unwrap().closed = true;
        self.shared.wake.notify_all();
    }
}

fn next_job(shared: &Shared) -> Option<usize> {
    let mut queue = shared.queue.lock().unwrap();
    loop {
        if queue.closed {
            return None;
        }
        if let Some(index) = queue.waiting.pop_front() {
            queue.running.insert(index);
            return Some(index);
        }
        queue = shared.wake.wait(queue).unwrap();
    }
}

fn worker(
    shared: Arc<Shared>,
    tx: Sender<ThumbnailResult>,
    ctx: egui::Context,
    archive: Arc<Archive>,
    side: u32,
) {
    let mut reader = None;
    while let Some(index) = next_job(&shared) {
        let result = make_thumbnail(&archive, &mut reader, index, side).map_err(|e| format!("{e:#}"));
        if tx.send(ThumbnailResult { index, result }).is_err() {
            return;
        }
        ctx.request_repaint();
    }
}

fn make_thumbnail(
    archive: &Archive,
    reader: &mut Option<ArchiveReader>,
    index: usize,
    side: u32,
) -> anyhow::Result<ThumbnailImage> {
    let entry = archive.entries.get(index).context("zip 항목이 없습니다")?;
    let reader = match reader {
        Some(reader) => reader,
        empty => empty.insert(ArchiveReader::open(&archive.path)?),
    };
    decode_thumbnail(reader.read_entry(entry.zip_index)?, &entry.path, side)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::test_support::{build_zip, temp_file};
    use std::io::Cursor;
    use std::time::{Duration, Instant};

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut out = Vec::new();
        image::RgbImage::new(width, height)
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn makes_thumbnails_and_reports_broken_entries() {
        let path = temp_file(
            "pool",
            &build_zip(&[("a.png", &png(400, 200)), ("b.png", b"broken"), ("c.png", &png(20, 10))]),
        );
        let archive = Arc::new(Archive::open(&path).unwrap());
        let pool = ThumbnailPool::spawn(egui::Context::default(), archive, 100);
        pool.set_wanted(&[0, 1, 2]);

        let mut results = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        while results.len() < 3 && Instant::now() < deadline {
            match pool.try_recv() {
                Some(result) => results.push(result),
                None => thread::sleep(Duration::from_millis(5)),
            }
        }
        drop(pool);
        std::fs::remove_file(&path).unwrap();

        results.sort_by_key(|result| result.index);
        assert_eq!(results.len(), 3);
        let large = results[0].result.as_ref().unwrap();
        assert_eq!((large.width, large.height), (100, 50));
        assert!(results[1].result.is_err());
        // 이미 작은 이미지는 확대하지 않습니다.
        let small = results[2].result.as_ref().unwrap();
        assert_eq!((small.width, small.height), (20, 10));
    }
}
