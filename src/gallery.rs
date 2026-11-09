//! zip 안의 이미지를 바둑판 형태로 보여 줍니다.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use egui::text::{LayoutJob, TextWrapping};
use egui::{
    Align2, Color32, CornerRadius, FontId, Pos2, Rect, Response, ScrollArea, Sense, Stroke,
    StrokeKind, TextureHandle, TextureOptions, Ui, UiBuilder, Vec2,
};

use crate::archive::Archive;
use crate::navigator::file_name;
use crate::thumbnails::{ThumbnailPool, ThumbnailResult};

/// 썸네일의 긴 변(픽셀). 200% 화면 배율에서도 칸을 채웁니다.
const THUMB_SIDE: u32 = 384;
const TILE_SIZE: f32 = 190.0;
const LABEL_HEIGHT: f32 = 26.0;
const ROW_HEIGHT: f32 = TILE_SIZE + LABEL_HEIGHT;
const GAP: f32 = 10.0;
const MARGIN: f32 = 12.0;
const SCROLLBAR_ALLOWANCE: f32 = 12.0;
/// 메모리에 둘 썸네일 수. 화면과 앞뒤 여유분보다 충분히 많아야 합니다.
const MAX_THUMBNAILS: usize = 600;
/// 한 프레임에 GPU로 올리는 수. 많이 몰려도 화면이 끊기지 않게 나눕니다.
const MAX_UPLOADS_PER_FRAME: usize = 8;

const TILE_BG: Color32 = Color32::from_rgb(40, 40, 44);
const LABEL: Color32 = Color32::from_rgb(190, 196, 204);
const MUTED: Color32 = Color32::from_rgb(154, 163, 173);
const ACCENT: Color32 = Color32::from_rgb(240, 134, 90);

enum Thumb {
    Ready { texture: TextureHandle, size: Vec2 },
    Failed,
}

pub struct Gallery {
    pub archive: Arc<Archive>,
    /// `false`면 한 장씩 보는 중입니다.
    pub visible: bool,
    korean_ui: bool,
    selected: usize,
    columns: usize,
    thumbs: HashMap<usize, Thumb>,
    /// 작업 스레드에 마지막으로 넘긴 목록.
    wanted: Vec<usize>,
    pool: ThumbnailPool,
    scroll_offset: f32,
    visible_center: usize,
    /// 화면과 앞뒤 여유분을 합친 칸 수. 이만큼은 제거하지 않습니다.
    window_len: usize,
    reveal_selected: bool,
}

impl Gallery {
    pub fn new(ctx: egui::Context, archive: Arc<Archive>, korean_ui: bool) -> Self {
        Self {
            pool: ThumbnailPool::spawn(ctx, archive.clone(), THUMB_SIDE),
            archive,
            visible: true,
            korean_ui,
            selected: 0,
            columns: 1,
            thumbs: HashMap::new(),
            wanted: Vec::new(),
            scroll_offset: 0.0,
            visible_center: 0,
            window_len: 0,
            reveal_selected: false,
        }
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// 선택을 옮기고 그 칸이 화면에 보이도록 스크롤합니다.
    pub fn select(&mut self, index: usize) {
        self.selected = index.min(self.archive.entries.len().saturating_sub(1));
        self.reveal_selected = true;
    }

    pub fn move_selection(&mut self, delta: isize) {
        self.select(self.selected.saturating_add_signed(delta));
    }

    pub fn move_rows(&mut self, rows: isize) {
        self.move_selection(rows * self.columns as isize);
    }

    /// 격자를 그리고, 클릭한 칸의 순번을 돌려줍니다.
    /// `on_tile`은 칸마다 호출되어 우클릭 메뉴를 붙이는 데 씁니다.
    pub fn show(&mut self, ui: &mut Ui, on_tile: &mut dyn FnMut(&Response)) -> Option<usize> {
        self.receive_thumbnails(ui.ctx());

        let view = ui.max_rect().shrink(MARGIN);
        let columns = columns_for(view.width() - SCROLLBAR_ALLOWANCE);
        self.columns = columns;
        let total = self.archive.entries.len();

        // id가 같으면 다른 zip을 열어도 이전 스크롤 위치가 이어집니다.
        let mut scroll = ScrollArea::vertical()
            .id_salt(&self.archive.path)
            .auto_shrink(false);
        if std::mem::take(&mut self.reveal_selected) {
            let row = self.selected / columns;
            if let Some(offset) = reveal_offset(row, self.scroll_offset, view.height()) {
                scroll = scroll.vertical_scroll_offset(offset);
            }
        }

        let mut activated = None;
        let mut right_clicked = None;
        let mut shown_rows = 0..0;
        let content_width = columns as f32 * (TILE_SIZE + GAP) - GAP;
        let output = ui
            .scope_builder(UiBuilder::new().max_rect(view), |ui| {
                ui.spacing_mut().item_spacing = Vec2::splat(GAP);
                scroll.show_rows(ui, ROW_HEIGHT, total.div_ceil(columns), |ui, rows| {
                    shown_rows = rows.clone();
                    for row in rows {
                        ui.horizontal(|ui| {
                            ui.add_space(((ui.available_width() - content_width) / 2.0).max(0.0));
                            for index in (row * columns)..((row + 1) * columns).min(total) {
                                let response = self.draw_tile(ui, index);
                                on_tile(&response);
                                if response.clicked() {
                                    activated = Some(index);
                                }
                                if response.secondary_clicked() {
                                    right_clicked = Some(index);
                                }
                            }
                        });
                    }
                })
            })
            .inner;
        self.scroll_offset = output.state.offset.y;
        // 우클릭 메뉴의 "선택한 이미지 보기"가 그 칸을 열도록 선택을 옮깁니다.
        if let Some(index) = right_clicked {
            self.selected = index;
        }

        self.request_thumbnails(shown_rows, columns);
        activated
    }

    fn draw_tile(&self, ui: &mut Ui, index: usize) -> Response {
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(TILE_SIZE, ROW_HEIGHT), Sense::click());
        let area = Rect::from_min_size(rect.min, Vec2::splat(TILE_SIZE));
        let painter = ui.painter();
        painter.rect_filled(area, CornerRadius::same(6), TILE_BG);

        match self.thumbs.get(&index) {
            Some(Thumb::Ready { texture, size }) => {
                let image = fit_rect(area.shrink(4.0), *size, ui.ctx().pixels_per_point());
                let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
                painter.image(texture.id(), image, uv, Color32::WHITE);
            }
            Some(Thumb::Failed) => {
                painter.text(
                    area.center(),
                    Align2::CENTER_CENTER,
                    if self.korean_ui { "표시할 수 없음" } else { "Cannot display" },
                    FontId::proportional(13.0),
                    MUTED,
                );
            }
            None => {}
        }

        if index == self.selected || response.hovered() {
            let color = if index == self.selected { ACCENT } else { MUTED };
            painter.rect_stroke(area, CornerRadius::same(6), Stroke::new(2.0, color), StrokeKind::Inside);
        }

        let mut job = LayoutJob::simple_singleline(
            file_name(&self.archive.entries[index].path),
            FontId::proportional(12.0),
            LABEL,
        );
        job.wrap = TextWrapping::truncate_at_width(TILE_SIZE - 8.0);
        let galley = painter.layout_job(job);
        let position = Pos2::new(rect.center().x - galley.size().x / 2.0, area.bottom() + 5.0);
        painter.galley(position, galley, LABEL);
        response
    }

    fn receive_thumbnails(&mut self, ctx: &egui::Context) {
        let mut received = 0;
        while received < MAX_UPLOADS_PER_FRAME {
            let Some(result) = self.pool.try_recv() else {
                break;
            };
            self.store_thumbnail(ctx, result);
            received += 1;
        }
        if received == MAX_UPLOADS_PER_FRAME {
            ctx.request_repaint();
        }
        if received > 0 {
            self.evict_far_thumbnails();
        }
    }

    fn store_thumbnail(&mut self, ctx: &egui::Context, thumbnail: ThumbnailResult) {
        let ThumbnailResult { index, result } = thumbnail;
        let thumb = match result {
            Ok(image) => {
                let size = [image.width as usize, image.height as usize];
                let pixels = egui::ColorImage::from_rgba_unmultiplied(size, &image.rgba);
                Thumb::Ready {
                    texture: ctx.load_texture(format!("thumb-{index}"), pixels, TextureOptions::LINEAR),
                    size: Vec2::new(image.width as f32, image.height as f32),
                }
            }
            Err(message) => {
                log::warn!("thumbnail failed for entry {index}: {message}");
                Thumb::Failed
            }
        };
        self.thumbs.insert(index, thumb);
    }

    fn evict_far_thumbnails(&mut self) {
        // 큰 화면에서는 필요한 칸이 상한보다 많을 수 있습니다. 그 칸까지 지우면
        // 지운 썸네일을 곧바로 다시 만드는 일이 끝없이 반복됩니다.
        let limit = MAX_THUMBNAILS.max(self.window_len);
        if self.thumbs.len() <= limit {
            return;
        }
        let mut indices: Vec<usize> = self.thumbs.keys().copied().collect();
        indices.sort_by_key(|index| index.abs_diff(self.visible_center));
        for index in &indices[limit..] {
            self.thumbs.remove(index);
        }
    }

    /// 보이는 칸을 먼저, 그다음 앞뒤로 화면 한 쪽 분량을 미리 만듭니다.
    fn request_thumbnails(&mut self, rows: Range<usize>, columns: usize) {
        let total = self.archive.entries.len();
        let first = (rows.start * columns).min(total);
        let end = (rows.end * columns).min(total);
        let margin = rows.len().max(1) * columns;
        let window_start = first.saturating_sub(margin);
        let window_end = (end + margin).min(total);
        self.visible_center = (first + end) / 2;
        self.window_len = window_end - window_start;

        let wanted: Vec<usize> = (first..end)
            .chain(end..window_end)
            .chain((window_start..first).rev())
            .filter(|index| !self.thumbs.contains_key(index))
            .collect();
        if wanted != self.wanted {
            self.pool.set_wanted(&wanted);
            self.wanted = wanted;
        }
    }
}

fn columns_for(width: f32) -> usize {
    (((width + GAP) / (TILE_SIZE + GAP)).floor() as usize).max(1)
}

/// `row`가 화면 밖이면 보이게 만드는 스크롤 위치를 돌려줍니다.
fn reveal_offset(row: usize, offset: f32, view_height: f32) -> Option<f32> {
    let top = row as f32 * (ROW_HEIGHT + GAP);
    let bottom = top + ROW_HEIGHT;
    if top < offset {
        Some(top)
    } else if bottom > offset + view_height {
        Some(bottom - view_height)
    } else {
        None
    }
}

/// 비율을 유지해 `area` 안에 맞춥니다. 작은 이미지는 흐려지지 않게 키우지 않습니다.
fn fit_rect(area: Rect, size: Vec2, pixels_per_point: f32) -> Rect {
    let scale = (area.width() / size.x)
        .min(area.height() / size.y)
        .min(1.0 / pixels_per_point);
    Rect::from_center_size(area.center(), size * scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_wheel_scrolls_the_grid() {
        use crate::archive::test_support::{build_zip, temp_file};
        use egui::{Event, Modifiers, MouseWheelUnit, RawInput, TouchPhase};

        let names: Vec<String> = (0..60).map(|i| format!("img{i}.png")).collect();
        let files: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &b"x"[..])).collect();
        let path = temp_file("gallery-scroll", &build_zip(&files));
        let archive = Arc::new(Archive::open(&path).unwrap());

        let ctx = egui::Context::default();
        let mut gallery = Gallery::new(ctx.clone(), archive, false);
        let mut events = vec![Event::PointerMoved(Pos2::new(300.0, 300.0))];
        for frame in 0..90 {
            if frame == 5 {
                events.push(Event::MouseWheel {
                    unit: MouseWheelUnit::Line,
                    delta: Vec2::new(0.0, -5.0),
                    phase: TouchPhase::Move,
                    modifiers: Modifiers::NONE,
                });
            }
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 700.0))),
                predicted_dt: 1.0 / 60.0,
                time: Some(f64::from(frame) / 60.0),
                events: std::mem::take(&mut events),
                ..Default::default()
            };
            ctx.run_ui(input, |ui| {
                gallery.show(ui, &mut |_| {});
            })
            .textures_delta
            .clear();
        }
        let offset = gallery.scroll_offset;
        drop(gallery);
        std::fs::remove_file(&path).unwrap();
        assert!(offset > 0.0, "scroll offset stayed at {offset}");
    }

    #[test]
    fn eviction_keeps_every_tile_the_window_needs() {
        use crate::archive::test_support::{build_zip, temp_file};

        let path = temp_file("gallery-evict", &build_zip(&[("a.png", b"x")]));
        let archive = Arc::new(Archive::open(&path).unwrap());
        let mut gallery = Gallery::new(egui::Context::default(), archive, false);
        std::fs::remove_file(&path).unwrap();

        gallery.thumbs = (0..1000).map(|index| (index, Thumb::Failed)).collect();
        gallery.visible_center = 500;
        gallery.window_len = 700;
        gallery.evict_far_thumbnails();
        assert_eq!(gallery.thumbs.len(), 700);
        assert!(gallery.thumbs.contains_key(&500));
        assert!(!gallery.thumbs.contains_key(&0));

        gallery.window_len = 10;
        gallery.evict_far_thumbnails();
        assert_eq!(gallery.thumbs.len(), MAX_THUMBNAILS);
    }

    #[test]
    fn column_count_follows_width_and_is_never_zero() {
        assert_eq!(columns_for(10.0), 1);
        assert_eq!(columns_for(TILE_SIZE), 1);
        assert_eq!(columns_for(TILE_SIZE * 2.0 + GAP), 2);
        assert_eq!(columns_for(TILE_SIZE * 2.0 + GAP - 1.0), 1);
    }

    #[test]
    fn reveal_scrolls_only_when_the_row_is_off_screen() {
        let step = ROW_HEIGHT + GAP;
        assert_eq!(reveal_offset(0, 0.0, 800.0), None);
        assert_eq!(reveal_offset(1, step * 3.0, 800.0), Some(step));
        assert_eq!(reveal_offset(10, 0.0, 800.0), Some(step * 10.0 + ROW_HEIGHT - 800.0));
    }

    #[test]
    fn thumbnails_shrink_to_fit_but_small_images_do_not_grow() {
        let area = Rect::from_min_size(Pos2::ZERO, Vec2::splat(100.0));
        let large = fit_rect(area, Vec2::new(400.0, 200.0), 1.0);
        assert_eq!((large.width(), large.height()), (100.0, 50.0));
        let small = fit_rect(area, Vec2::new(30.0, 30.0), 1.5);
        assert_eq!(small.width(), 20.0);
        assert_eq!(small.center(), area.center());
    }
}
