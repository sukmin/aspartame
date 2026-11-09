//! The egui application: state, input handling, drawing and window control.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{
    Align2, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, PointerButton,
    Pos2, Rect, Sense, Stroke, StrokeKind, TextureFilter, TextureHandle, TextureOptions,
    TextureWrapMode, Vec2, ViewportCommand,
};

use crate::image_loader::{DecodedImage, ImageLoader, LoadRequest, LoadResult, Priority};
use crate::input::{collect_actions, Action};
use crate::navigator::{file_name, Navigator, SUPPORTED_EXTENSIONS};
use crate::viewport::{Viewport, ZoomMode, ZOOM_STEP};
use crate::APP_NAME;

const BG: Color32 = Color32::from_rgb(28, 28, 30);
const TEXT: Color32 = Color32::from_rgb(230, 232, 234);
const MUTED: Color32 = Color32::from_rgb(154, 163, 173);
const ACCENT: Color32 = Color32::from_rgb(240, 134, 90);
const ERROR: Color32 = Color32::from_rgb(255, 120, 120);
const PANEL: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 170);

/// Upper bound for decoded textures kept alive (current image + neighbours).
const CACHE_BUDGET_BYTES: usize = 512 * 1024 * 1024;
const CACHE_MAX_ENTRIES: usize = 3;
/// Loads shorter than this show no spinner, to avoid flicker.
const SPINNER_DELAY: Duration = Duration::from_millis(150);
/// Seconds of pointer inactivity before the cursor is hidden in fullscreen.
const CURSOR_HIDE_AFTER: f64 = 2.0;
/// Used until the backend reports its real texture limit.
const FALLBACK_MAX_TEXTURE_SIDE: u32 = 8192;

struct CachedImage {
    texture: TextureHandle,
    /// Texture size (after any downscaling).
    width: u32,
    height: u32,
    /// Pixel size of the file.
    original_width: u32,
    original_height: u32,
    file_size: u64,
    /// Approximate CPU/GPU memory of the texture.
    bytes: usize,
}

impl CachedImage {
    fn size_vec2(&self) -> Vec2 {
        Vec2::new(self.width as f32, self.height as f32)
    }

    fn is_downscaled(&self) -> bool {
        self.width != self.original_width || self.height != self.original_height
    }
}

/// Textures keyed by path, least recently used first.
#[derive(Default)]
struct TextureCache {
    entries: Vec<(PathBuf, CachedImage)>,
}

impl TextureCache {
    fn get(&self, path: &Path) -> Option<&CachedImage> {
        self.entries.iter().find(|(p, _)| p == path).map(|(_, img)| img)
    }

    fn contains(&self, path: &Path) -> bool {
        self.entries.iter().any(|(p, _)| p == path)
    }

    fn touch(&mut self, path: &Path) {
        if let Some(i) = self.entries.iter().position(|(p, _)| p == path) {
            let entry = self.entries.remove(i);
            self.entries.push(entry);
        }
    }

    fn insert(&mut self, path: PathBuf, image: CachedImage) {
        self.entries.retain(|(p, _)| p != &path);
        self.entries.push((path, image));
    }

    fn total_bytes(&self) -> usize {
        self.entries.iter().map(|(_, img)| img.bytes).sum()
    }

    /// Drops the oldest entries until the limits are met, never touching `keep`.
    fn evict(&mut self, keep: &[&Path]) {
        loop {
            let over_limit = self.entries.len() > CACHE_MAX_ENTRIES
                || self.total_bytes() > CACHE_BUDGET_BYTES;
            if !over_limit {
                break;
            }
            let Some(i) = self
                .entries
                .iter()
                .position(|(p, _)| !keep.contains(&p.as_path()))
            else {
                break;
            };
            self.entries.remove(i);
        }
    }
}

struct PendingLoad {
    path: PathBuf,
    started: Instant,
}

pub struct ViewerApp {
    navigator: Navigator,
    viewport: Viewport,
    loader: ImageLoader,
    cache: TextureCache,
    /// The image currently drawn; always present in `cache`.
    shown: Option<PathBuf>,
    /// The image the user is waiting for.
    pending: Option<PendingLoad>,
    /// Bumped on every navigation so stale loader results can be ignored.
    generation: u64,
    error: Option<String>,
    fullscreen: bool,
    show_info: bool,
    /// A Korean-capable font was found, so UI labels can be Korean.
    korean_ui: bool,
    max_texture_side: u32,
    /// File from the command line, opened on the first frame.
    initial_path: Option<PathBuf>,
    last_title: String,
    last_activity: f64,
    cursor_hidden: bool,
}

impl ViewerApp {
    pub fn new(cc: &eframe::CreationContext<'_>, initial_path: Option<PathBuf>) -> Self {
        let ctx = &cc.egui_ctx;
        ctx.set_theme(egui::ThemePreference::Dark);
        // Ctrl +/-/0 would otherwise rescale the whole UI instead of the image.
        ctx.options_mut(|options| options.zoom_with_keyboard = false);
        let korean_ui = install_korean_font(ctx);

        Self {
            navigator: Navigator::new(),
            viewport: Viewport::new(),
            loader: ImageLoader::spawn(ctx.clone()),
            cache: TextureCache::default(),
            shown: None,
            pending: None,
            generation: 0,
            error: None,
            fullscreen: false,
            show_info: false,
            korean_ui,
            max_texture_side: FALLBACK_MAX_TEXTURE_SIDE,
            initial_path,
            last_title: String::new(),
            last_activity: 0.0,
            cursor_hidden: false,
        }
    }

    /// Korean label when a Korean font is available, English otherwise.
    fn t<'a>(&self, ko: &'a str, en: &'a str) -> &'a str {
        if self.korean_ui {
            ko
        } else {
            en
        }
    }

    // ----- opening and navigation -------------------------------------------------

    fn open_path(&mut self, ctx: &egui::Context, path: &Path) {
        self.navigator.open(path);
        self.error = None;
        self.sync_target(ctx);
    }

    fn open_dialog(&mut self, ctx: &egui::Context) {
        let mut dialog = rfd::FileDialog::new()
            .set_title(self.t("이미지 열기", "Open image"))
            .add_filter(self.t("이미지 파일", "Image files"), SUPPORTED_EXTENSIONS)
            .add_filter(self.t("모든 파일", "All files"), &["*"]);
        if let Some(dir) = self.navigator.dir() {
            dialog = dialog.set_directory(dir);
        }
        if let Some(path) = dialog.pick_file() {
            self.open_path(ctx, &path);
        }
    }

    fn navigate(&mut self, ctx: &egui::Context, delta: isize) {
        if self.navigator.step(delta).is_some() {
            self.sync_target(ctx);
        }
    }

    /// Makes the navigator's current image the one being shown: from the cache
    /// when possible, otherwise through the loader.
    fn sync_target(&mut self, ctx: &egui::Context) {
        self.generation += 1;
        let Some(target) = self.navigator.current().map(Path::to_path_buf) else {
            self.shown = None;
            self.pending = None;
            return;
        };

        if self.cache.contains(&target) {
            self.pending = None;
            self.cache.touch(&target);
            self.show(ctx, target);
        } else {
            self.pending = Some(PendingLoad {
                path: target.clone(),
                started: Instant::now(),
            });
            self.loader.request(LoadRequest {
                path: target,
                generation: self.generation,
                priority: Priority::Primary,
                max_side: self.max_texture_side,
            });
            ctx.request_repaint_after(SPINNER_DELAY);
        }
    }

    fn show(&mut self, ctx: &egui::Context, path: PathBuf) {
        if self.shown.as_ref() != Some(&path) {
            // Every newly shown image starts out fitted to the window.
            self.viewport.reset_fit();
        }
        self.shown = Some(path);
        self.error = None;
        self.request_preloads();
        self.evict_cache();
        ctx.request_repaint();
    }

    fn handle_result(&mut self, ctx: &egui::Context, result: LoadResult) {
        let LoadResult {
            path,
            generation,
            priority,
            result,
        } = result;
        let is_current_primary = priority == Priority::Primary && generation == self.generation;
        let awaited = self.pending.as_ref().is_some_and(|p| p.path == path);

        match result {
            Ok(image) => {
                let cached = upload_texture(ctx, image);
                self.cache.insert(path.clone(), cached);
                if is_current_primary || awaited {
                    self.pending = None;
                    self.show(ctx, path);
                } else {
                    self.evict_cache();
                }
            }
            Err(message) => {
                if !(is_current_primary || awaited) {
                    log::warn!("preload failed for {}: {message}", path.display());
                    return;
                }
                self.pending = None;
                if !path.exists() {
                    // The file vanished: rescan and jump to whatever took its place.
                    self.navigator.refresh();
                    if let Some(replacement) = self.navigator.current() {
                        if replacement != path {
                            self.sync_target(ctx);
                            return;
                        }
                    }
                }
                log::error!("failed to load {}: {message}", path.display());
                self.shown = None;
                self.error = Some(format!("{}\n{message}", file_name(&path)));
            }
        }
    }

    fn request_preloads(&self) {
        let mut requested: Vec<PathBuf> = Vec::new();
        for delta in [1isize, -1] {
            let Some(path) = self.navigator.peek(delta) else {
                continue;
            };
            if self.shown.as_deref() == Some(path)
                || self.cache.contains(path)
                || requested.iter().any(|p| p == path)
            {
                continue;
            }
            self.loader.request(LoadRequest {
                path: path.to_path_buf(),
                generation: self.generation,
                priority: Priority::Preload,
                max_side: self.max_texture_side,
            });
            requested.push(path.to_path_buf());
        }
    }

    fn evict_cache(&mut self) {
        let keep: Vec<PathBuf> = self
            .shown
            .iter()
            .cloned()
            .chain(self.navigator.current().map(Path::to_path_buf))
            .chain(self.navigator.peek(1).map(Path::to_path_buf))
            .chain(self.navigator.peek(-1).map(Path::to_path_buf))
            .collect();
        let keep: Vec<&Path> = keep.iter().map(PathBuf::as_path).collect();
        self.cache.evict(&keep);
    }

    // ----- actions ------------------------------------------------------------------

    fn apply(
        &mut self,
        ctx: &egui::Context,
        action: Action,
        view: Rect,
        image_size: Option<Vec2>,
        hover: Option<Pos2>,
        pixels_per_point: f32,
    ) {
        match action {
            Action::Open => self.open_dialog(ctx),
            Action::OpenPaths(paths) => {
                if let Some(path) = paths.into_iter().find(|p| p.is_file()) {
                    self.open_path(ctx, &path);
                }
            }
            Action::Next => self.navigate(ctx, 1),
            Action::Prev => self.navigate(ctx, -1),
            Action::First => {
                if self.navigator.first() {
                    self.sync_target(ctx);
                }
            }
            Action::Last => {
                if self.navigator.last() {
                    self.sync_target(ctx);
                }
            }
            Action::Fit => self.viewport.reset_fit(),
            Action::ActualSize => self.viewport.set_actual(),
            Action::ZoomIn => self.zoom(ZOOM_STEP, None, view, image_size, pixels_per_point),
            Action::ZoomOut => self.zoom(1.0 / ZOOM_STEP, None, view, image_size, pixels_per_point),
            Action::ZoomBy(factor) => self.zoom(factor, hover, view, image_size, pixels_per_point),
            Action::ToggleFullscreen => self.set_fullscreen(ctx, !self.fullscreen),
            Action::Escape => {
                if self.fullscreen {
                    self.set_fullscreen(ctx, false);
                } else {
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
            Action::Refresh => {
                self.navigator.refresh();
                self.sync_target(ctx);
            }
            Action::ToggleInfo => self.show_info = !self.show_info,
        }
    }

    fn zoom(
        &mut self,
        factor: f32,
        anchor: Option<Pos2>,
        view: Rect,
        image_size: Option<Vec2>,
        pixels_per_point: f32,
    ) {
        if let Some(size) = image_size {
            self.viewport.zoom_by(factor, anchor, view, size, pixels_per_point);
        }
    }

    fn set_fullscreen(&mut self, ctx: &egui::Context, on: bool) {
        if self.fullscreen == on {
            return;
        }
        self.fullscreen = on;
        ctx.send_viewport_cmd(ViewportCommand::Fullscreen(on));
        if !on && self.cursor_hidden {
            ctx.send_viewport_cmd(ViewportCommand::CursorVisible(true));
            self.cursor_hidden = false;
        }
    }

    /// Hides the mouse cursor after a while in fullscreen; shows it on activity.
    fn update_cursor(&mut self, ctx: &egui::Context) {
        let (time, active) = ctx.input(|i| {
            let active = i.pointer.is_moving() || i.pointer.any_down() || !i.events.is_empty();
            (i.time, active)
        });
        if active {
            self.last_activity = time;
        }
        if !self.fullscreen {
            return;
        }
        let idle = time - self.last_activity;
        if active && self.cursor_hidden {
            ctx.send_viewport_cmd(ViewportCommand::CursorVisible(true));
            self.cursor_hidden = false;
        } else if !active && !self.cursor_hidden && idle >= CURSOR_HIDE_AFTER {
            ctx.send_viewport_cmd(ViewportCommand::CursorVisible(false));
            self.cursor_hidden = true;
        } else if !self.cursor_hidden {
            let wait = (CURSOR_HIDE_AFTER - idle).max(0.05);
            ctx.request_repaint_after(Duration::from_secs_f64(wait));
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let pixels_per_point = ctx.pixels_per_point();
        let shown = self
            .shown
            .as_ref()
            .and_then(|p| self.cache.get(p).map(|img| (p, img)));
        let title = match shown {
            Some((path, img)) => {
                let position = self
                    .navigator
                    .position()
                    .map(|(i, n)| format!(" [{i}/{n}]"))
                    .unwrap_or_default();
                format!(
                    "{} — {}×{} — {:.0}%{} — {APP_NAME}",
                    file_name(path),
                    img.original_width,
                    img.original_height,
                    self.viewport.percent(pixels_per_point),
                    position,
                )
            }
            None => match (&self.pending, self.navigator.current()) {
                (Some(pending), _) => format!("{} — {APP_NAME}", file_name(&pending.path)),
                (None, Some(current)) if self.error.is_some() => {
                    format!("{} — {} — {APP_NAME}", file_name(current), self.t("오류", "error"))
                }
                _ => APP_NAME.to_owned(),
            },
        };
        if title != self.last_title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
    }

    // ----- drawing ------------------------------------------------------------------

    fn canvas(&mut self, ui: &mut egui::Ui, mut actions: Vec<Action>) {
        let ctx = ui.ctx().clone();
        let view = ui.max_rect();
        let pixels_per_point = ctx.pixels_per_point();
        let response = ui.interact(view, ui.id().with("canvas"), Sense::click_and_drag());
        let hover = ctx.input(|i| i.pointer.hover_pos());
        // Escape while the context menu is open should only close the menu, not the app.
        let menu_was_open = egui::containers::Popup::is_any_open(&ctx);

        let image_size = self
            .shown
            .as_ref()
            .and_then(|p| self.cache.get(p))
            .map(CachedImage::size_vec2);
        // Make `scale` reflect the current mode before zooming relative to it.
        if let Some(size) = image_size {
            self.viewport.layout(view, size, pixels_per_point);
        }

        self.context_menu(&response, &mut actions);
        if response.dragged_by(PointerButton::Primary) {
            if let Some(size) = image_size {
                self.viewport.pan_by(response.drag_delta(), view, size);
            }
        }
        if response.double_clicked() {
            actions.push(Action::ToggleFullscreen);
        }
        for action in actions {
            if menu_was_open && action == Action::Escape {
                continue;
            }
            self.apply(&ctx, action, view, image_size, hover, pixels_per_point);
        }

        let painter = ui.painter().clone();
        let mut drawn = false;
        if let Some(path) = self.shown.clone() {
            if let Some(img) = self.cache.get(&path) {
                let rect = self.viewport.layout(view, img.size_vec2(), pixels_per_point);
                let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
                painter.image(img.texture.id(), rect, uv, Color32::WHITE);
                drawn = true;
            }
        }

        if let Some(message) = &self.error {
            self.draw_error(&painter, view, message);
        } else if !drawn && self.pending.is_none() {
            self.draw_empty_hint(&painter, view);
        }
        if self.show_info {
            self.draw_info(&painter, view, pixels_per_point);
        }
        self.draw_loading(ui, view);
        self.draw_drop_hint(&painter, view, &ctx);
    }

    fn context_menu(&self, response: &egui::Response, actions: &mut Vec<Action>) {
        // `None` is a separator.
        let items: Vec<Option<(String, Action)>> = vec![
            Some((format!("{}   Ctrl+O", self.t("열기...", "Open...")), Action::Open)),
            None,
            Some((format!("{}   ←", self.t("이전 이미지", "Previous image")), Action::Prev)),
            Some((format!("{}   →", self.t("다음 이미지", "Next image")), Action::Next)),
            None,
            Some((format!("{}   F", self.t("창에 맞춤", "Fit to window")), Action::Fit)),
            Some((format!("{}   1", self.t("실제 크기", "Actual size")), Action::ActualSize)),
            Some((format!("{}   +", self.t("확대", "Zoom in")), Action::ZoomIn)),
            Some((format!("{}   -", self.t("축소", "Zoom out")), Action::ZoomOut)),
            None,
            Some((format!("{}   F11", self.t("전체 화면", "Fullscreen")), Action::ToggleFullscreen)),
            Some((format!("{}   I", self.t("정보 표시", "Show info")), Action::ToggleInfo)),
            Some((format!("{}   F5", self.t("폴더 새로 고침", "Refresh folder")), Action::Refresh)),
            None,
            Some((format!("{}   Esc", self.t("종료", "Quit")), Action::Escape)),
        ];
        response.context_menu(|ui| {
            for item in &items {
                match item {
                    None => {
                        ui.separator();
                    }
                    Some((label, action)) => {
                        if ui.button(label.as_str()).clicked() {
                            actions.push(action.clone());
                            ui.close();
                        }
                    }
                }
            }
        });
    }

    fn draw_error(&self, painter: &egui::Painter, view: Rect, message: &str) {
        let mut y = view.center().y - 24.0;
        for (i, line) in message.lines().enumerate() {
            let (size, color) = if i == 0 { (18.0, TEXT) } else { (14.0, ERROR) };
            let rect = painter.text(
                Pos2::new(view.center().x, y),
                Align2::CENTER_TOP,
                line,
                FontId::proportional(size),
                color,
            );
            y = rect.max.y + 6.0;
        }
    }

    fn draw_empty_hint(&self, painter: &egui::Painter, view: Rect) {
        painter.text(
            view.center() - Vec2::new(0.0, 14.0),
            Align2::CENTER_CENTER,
            self.t(
                "이미지 파일을 여기에 끌어다 놓거나 Ctrl+O로 여세요",
                "Drop an image here or press Ctrl+O",
            ),
            FontId::proportional(18.0),
            MUTED,
        );
        painter.text(
            view.center() + Vec2::new(0.0, 16.0),
            Align2::CENTER_CENTER,
            self.t(
                "← → 이전/다음    F 창에 맞춤    1 실제 크기    F11 전체 화면    I 정보",
                "Left/Right prev/next    F fit    1 actual size    F11 fullscreen    I info",
            ),
            FontId::proportional(13.0),
            MUTED,
        );
    }

    fn draw_info(&self, painter: &egui::Painter, view: Rect, pixels_per_point: f32) {
        let Some((path, img)) = self
            .shown
            .as_ref()
            .and_then(|p| self.cache.get(p).map(|img| (p, img)))
        else {
            return;
        };

        let mode = match self.viewport.mode {
            ZoomMode::Fit => self.t("창에 맞춤", "fit"),
            ZoomMode::Actual => self.t("실제 크기", "actual size"),
            ZoomMode::Custom => self.t("사용자 배율", "custom"),
        };
        let mut lines: Vec<(String, Color32)> = vec![
            (file_name(path), TEXT),
            (
                path.parent()
                    .map(|d| d.display().to_string())
                    .unwrap_or_default(),
                MUTED,
            ),
            (
                format!("{} × {} px", img.original_width, img.original_height),
                TEXT,
            ),
        ];
        if img.is_downscaled() {
            lines.push((
                format!(
                    "{} {}×{}",
                    self.t("표시용 축소:", "Downscaled for display:"),
                    img.width,
                    img.height
                ),
                MUTED,
            ));
        }
        lines.push((format_size(img.file_size), MUTED));
        lines.push((
            format!("{:.0}%  ({mode})", self.viewport.percent(pixels_per_point)),
            ACCENT,
        ));
        if let Some((i, n)) = self.navigator.position() {
            lines.push((format!("{i} / {n}"), MUTED));
        }

        let font = FontId::proportional(14.0);
        let galleys: Vec<_> = lines
            .into_iter()
            .map(|(text, color)| painter.layout_no_wrap(text, font.clone(), color))
            .collect();
        let line_gap = 3.0;
        let width = galleys.iter().map(|g| g.size().x).fold(0.0f32, f32::max);
        let height = galleys.iter().map(|g| g.size().y + line_gap).sum::<f32>() - line_gap;
        let pad = 10.0;
        let rect = Rect::from_min_size(
            view.min + Vec2::splat(12.0),
            Vec2::new(width + 2.0 * pad, height + 2.0 * pad),
        );
        painter.rect_filled(rect, CornerRadius::same(6), PANEL);
        let mut y = rect.min.y + pad;
        for galley in galleys {
            let h = galley.size().y;
            painter.galley(Pos2::new(rect.min.x + pad, y), galley, TEXT);
            y += h + line_gap;
        }
    }

    fn draw_loading(&self, ui: &mut egui::Ui, view: Rect) {
        let Some(pending) = &self.pending else {
            return;
        };
        if pending.started.elapsed() < SPINNER_DELAY {
            ui.ctx().request_repaint_after(SPINNER_DELAY);
            return;
        }
        let rect = Rect::from_center_size(view.max - Vec2::splat(28.0), Vec2::splat(28.0));
        ui.put(rect, egui::Spinner::new().size(24.0).color(ACCENT));
    }

    fn draw_drop_hint(&self, painter: &egui::Painter, view: Rect, ctx: &egui::Context) {
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        if !hovering {
            return;
        }
        painter.rect_stroke(
            view.shrink(6.0),
            CornerRadius::same(10),
            Stroke::new(3.0, ACCENT),
            StrokeKind::Inside,
        );
        painter.text(
            view.center(),
            Align2::CENTER_CENTER,
            self.t("놓아서 열기", "Drop to open"),
            FontId::proportional(24.0),
            ACCENT,
        );
    }
}

impl eframe::App for ViewerApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        BG.to_normalized_gamma_f32()
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let side = ctx.input(|i| i.max_texture_side);
        if side >= 256 {
            self.max_texture_side = side as u32;
        }
        if let Some(path) = self.initial_path.take() {
            self.open_path(ctx, &path);
        }
        while let Some(result) = self.loader.try_recv() {
            self.handle_result(ctx, result);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let actions = ctx.input(collect_actions);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(BG))
            .show(ui, |ui| self.canvas(ui, actions));
        self.update_title(&ctx);
        self.update_cursor(&ctx);
    }
}

/// Uploads decoded pixels to the GPU with mipmaps, so heavily downscaled
/// photos do not shimmer.
fn upload_texture(ctx: &egui::Context, image: DecodedImage) -> CachedImage {
    let bytes = image.rgba.len();
    let color_image = egui::ColorImage::from_rgba_unmultiplied(
        [image.width as usize, image.height as usize],
        &image.rgba,
    );
    let options = TextureOptions {
        magnification: TextureFilter::Linear,
        minification: TextureFilter::Linear,
        wrap_mode: TextureWrapMode::ClampToEdge,
        mipmap_mode: Some(TextureFilter::Linear),
    };
    let texture = ctx.load_texture(file_name(&image.path), color_image, options);
    CachedImage {
        texture,
        width: image.width,
        height: image.height,
        original_width: image.original_width,
        original_height: image.original_height,
        file_size: image.file_size,
        bytes,
    }
}

/// egui's bundled fonts have no Hangul glyphs, so append a Windows system font
/// as a fallback. Returns whether one was found.
fn install_korean_font(ctx: &egui::Context) -> bool {
    let fonts_dir = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:\\Windows"))
        .join("Fonts");
    for name in ["malgun.ttf", "malgunbd.ttf", "gulim.ttc"] {
        let path = fonts_dir.join(name);
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let mut fonts = FontDefinitions::default();
        fonts
            .font_data
            .insert("korean".to_owned(), Arc::new(FontData::from_owned(bytes)));
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            fonts
                .families
                .entry(family)
                .or_default()
                .push("korean".to_owned());
        }
        ctx.set_fonts(fonts);
        log::info!("Korean UI font loaded from {}", path.display());
        return true;
    }
    log::warn!("no Korean font found; using English UI labels");
    false
}

fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b >= KB * KB * KB {
        format!("{:.2} GB", b / (KB * KB * KB))
    } else if b >= KB * KB {
        format!("{:.1} MB", b / (KB * KB))
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}
