//! Folder navigation: scans the folder of the opened image, sorts the image
//! files by name (case-insensitive natural order, like Windows Explorer) and
//! moves between them.

use std::cmp::Ordering;
use std::iter::Peekable;
use std::path::{Path, PathBuf};
use std::str::Chars;

/// File extensions (lower-case) that are listed when scanning a folder.
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "jpe", "jfif", "bmp", "gif", "webp", "tif", "tiff", "ico", "tga",
];

/// Whether the file has one of the supported image extensions (case-insensitive).
pub fn is_supported(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            let ext = ext.to_ascii_lowercase();
            SUPPORTED_EXTENSIONS.contains(&ext.as_str())
        })
        .unwrap_or(false)
}

fn take_digits(chars: &mut Peekable<Chars<'_>>) -> String {
    let mut digits = String::new();
    while let Some(c) = chars.peek() {
        if c.is_ascii_digit() {
            digits.push(*c);
            chars.next();
        } else {
            break;
        }
    }
    digits
}

fn lowercase(c: char) -> String {
    c.to_lowercase().collect()
}

/// Case-insensitive natural comparison: digit runs are compared by numeric
/// value, so `img2.png` sorts before `img10.png`. Ties fall back to a plain
/// byte comparison so the order is always deterministic.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ca), Some(cb)) => {
                if ca.is_ascii_digit() && cb.is_ascii_digit() {
                    let na = take_digits(&mut ai);
                    let nb = take_digits(&mut bi);
                    let ta = na.trim_start_matches('0');
                    let tb = nb.trim_start_matches('0');
                    let ord = ta
                        .len()
                        .cmp(&tb.len())
                        .then_with(|| ta.cmp(tb))
                        // Same numeric value: fewer leading zeros first ("1" < "01").
                        .then_with(|| na.len().cmp(&nb.len()));
                    if ord != Ordering::Equal {
                        return ord;
                    }
                } else {
                    match lowercase(ca).cmp(&lowercase(cb)) {
                        Ordering::Equal => {
                            ai.next();
                            bi.next();
                        }
                        ord => return ord,
                    }
                }
            }
        }
    }
}

pub fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Orders two paths by their file names using [`natural_cmp`].
pub fn compare_paths(a: &Path, b: &Path) -> Ordering {
    natural_cmp(&file_name(a), &file_name(b))
}

/// Two paths in the same folder refer to the same file if the names match
/// case-insensitively (Windows file systems are case-insensitive).
fn same_file(a: &Path, b: &Path) -> bool {
    file_name(a).to_lowercase() == file_name(b).to_lowercase()
}

/// Lists the supported image files directly inside `dir`, sorted by name.
pub fn scan_dir(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && is_supported(path))
        .collect();
    files.sort_by(|a, b| compare_paths(a, b));
    Ok(files)
}

/// Keeps the sorted list of images in the current folder and the index of the
/// image being shown.
#[derive(Debug, Default)]
pub struct Navigator {
    dir: Option<PathBuf>,
    files: Vec<PathBuf>,
    index: Option<usize>,
    /// Wrap around from the last image to the first one and vice versa.
    pub wrap: bool,
}

impl Navigator {
    pub fn new() -> Self {
        Self {
            wrap: true,
            ..Default::default()
        }
    }

    /// Opens `path`: scans its folder and selects it. A file whose extension
    /// is not in [`SUPPORTED_EXTENSIONS`] is still inserted into the list at
    /// its sorted position, so the user can view it and move on from it.
    pub fn open(&mut self, path: &Path) {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let dir = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));

        let mut files = match scan_dir(&dir) {
            Ok(files) => files,
            Err(err) => {
                log::warn!("cannot scan folder {}: {err}", dir.display());
                Vec::new()
            }
        };

        let index = match files.iter().position(|f| same_file(f, &path)) {
            Some(i) => i,
            None => {
                let i = files.partition_point(|f| compare_paths(f, &path) == Ordering::Less);
                files.insert(i, path.clone());
                i
            }
        };

        self.dir = Some(dir);
        self.files = files;
        self.index = Some(index);
    }

    pub fn current(&self) -> Option<&Path> {
        self.index.and_then(|i| self.files.get(i)).map(PathBuf::as_path)
    }

    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.files.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// 1-based position of the current image and the total count.
    pub fn position(&self) -> Option<(usize, usize)> {
        self.index.map(|i| (i + 1, self.files.len()))
    }

    fn target_index(&self, delta: isize) -> Option<usize> {
        let len = self.files.len();
        let idx = self.index?;
        if len <= 1 {
            return None;
        }
        let raw = idx as isize + delta;
        let new = if self.wrap {
            raw.rem_euclid(len as isize) as usize
        } else {
            raw.clamp(0, len as isize - 1) as usize
        };
        (new != idx).then_some(new)
    }

    /// The image `delta` steps away from the current one, without moving.
    pub fn peek(&self, delta: isize) -> Option<&Path> {
        self.target_index(delta)
            .and_then(|i| self.files.get(i))
            .map(PathBuf::as_path)
    }

    /// Moves `delta` images forward (or backward when negative).
    /// Returns the new current image, or `None` when nothing changed.
    pub fn step(&mut self, delta: isize) -> Option<&Path> {
        let new = self.target_index(delta)?;
        self.index = Some(new);
        self.current()
    }

    /// Jumps to the first image. Returns `true` when the selection changed.
    pub fn first(&mut self) -> bool {
        self.jump_to(0)
    }

    /// Jumps to the last image. Returns `true` when the selection changed.
    pub fn last(&mut self) -> bool {
        self.jump_to(self.files.len().saturating_sub(1))
    }

    fn jump_to(&mut self, index: usize) -> bool {
        if self.files.is_empty() || self.index == Some(index) {
            return false;
        }
        self.index = Some(index);
        true
    }

    /// Re-scans the folder. Keeps the current image selected if it still
    /// exists; otherwise selects the image that now occupies its position.
    pub fn refresh(&mut self) {
        let Some(dir) = self.dir.clone() else {
            return;
        };
        let current = self.current().map(Path::to_path_buf);
        let files = match scan_dir(&dir) {
            Ok(files) => files,
            Err(err) => {
                log::warn!("cannot rescan folder {}: {err}", dir.display());
                return;
            }
        };

        let index = if files.is_empty() {
            None
        } else {
            match &current {
                Some(cur) => files
                    .iter()
                    .position(|f| same_file(f, cur))
                    .or_else(|| {
                        let i = files.partition_point(|f| compare_paths(f, cur) == Ordering::Less);
                        Some(i.min(files.len() - 1))
                    }),
                None => Some(0),
            }
        };

        self.files = files;
        self.index = index;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(names: &[&str]) -> Vec<String> {
        let mut v: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        v.sort_by(|a, b| natural_cmp(a, b));
        v
    }

    #[test]
    fn numbers_sort_by_value() {
        assert_eq!(
            sorted(&["img10.png", "img2.png", "img1.png"]),
            vec!["img1.png", "img2.png", "img10.png"]
        );
    }

    #[test]
    fn case_is_ignored() {
        assert_eq!(sorted(&["b.png", "A.png", "c.png"]), vec!["A.png", "b.png", "c.png"]);
        assert_eq!(
            sorted(&["photo10.jpg", "Photo2.jpg", "photo1.jpg"]),
            vec!["photo1.jpg", "Photo2.jpg", "photo10.jpg"]
        );
    }

    #[test]
    fn leading_zeros_and_prefix() {
        assert_eq!(sorted(&["01.png", "1.png", "002.png"]), vec!["1.png", "01.png", "002.png"]);
        assert_eq!(sorted(&["a1.png", "a.png"]), vec!["a.png", "a1.png"]);
    }

    #[test]
    fn hangul_sorts_after_latin_in_codepoint_order() {
        assert_eq!(
            sorted(&["나.png", "가.png", "z.png"]),
            vec!["z.png", "가.png", "나.png"]
        );
    }

    #[test]
    fn extension_filter_is_case_insensitive() {
        assert!(is_supported(Path::new("a.PNG")));
        assert!(is_supported(Path::new("a.JpEg")));
        assert!(!is_supported(Path::new("a.txt")));
        assert!(!is_supported(Path::new("noext")));
    }

    fn temp_folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aspartame-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn scans_and_navigates_folder() {
        let dir = temp_folder("nav");
        for name in ["img10.png", "img2.png", "Img1.PNG", "b.jpg", "가.png", "readme.txt", "z.webp"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }

        let mut nav = Navigator::new();
        nav.open(&dir.join("img2.png"));

        let names: Vec<String> = scan_dir(&dir).unwrap().iter().map(|p| file_name(p)).collect();
        assert_eq!(names, vec!["b.jpg", "Img1.PNG", "img2.png", "img10.png", "z.webp", "가.png"]);

        assert_eq!(nav.position(), Some((3, 6)));
        assert_eq!(file_name(nav.step(1).unwrap()), "img10.png");
        assert_eq!(file_name(nav.step(-2).unwrap()), "Img1.PNG");
        assert_eq!(file_name(nav.peek(-2).unwrap()), "가.png"); // wraps around
        assert!(nav.last());
        assert_eq!(file_name(nav.current().unwrap()), "가.png");
        assert_eq!(file_name(nav.step(1).unwrap()), "b.jpg"); // wraps around
        nav.wrap = false;
        assert!(nav.step(-1).is_none());

        // Unsupported extension opened explicitly is inserted into the list.
        nav.open(&dir.join("readme.txt"));
        assert_eq!(nav.position(), Some((5, 7)));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refresh_handles_deleted_current_file() {
        let dir = temp_folder("refresh");
        for name in ["1.png", "2.png", "3.png"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        let mut nav = Navigator::new();
        nav.open(&dir.join("2.png"));
        std::fs::remove_file(dir.join("2.png")).unwrap();
        nav.refresh();
        assert_eq!(file_name(nav.current().unwrap()), "3.png");
        assert_eq!(nav.len(), 2);

        std::fs::remove_file(dir.join("3.png")).unwrap();
        nav.refresh();
        assert_eq!(file_name(nav.current().unwrap()), "1.png");

        std::fs::remove_file(dir.join("1.png")).unwrap();
        nav.refresh();
        assert!(nav.current().is_none());
        assert!(nav.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn single_image_does_not_move() {
        let dir = temp_folder("single");
        std::fs::write(dir.join("only.png"), b"x").unwrap();
        let mut nav = Navigator::new();
        nav.open(&dir.join("only.png"));
        assert!(nav.step(1).is_none());
        assert!(nav.step(-1).is_none());
        assert!(nav.peek(1).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
