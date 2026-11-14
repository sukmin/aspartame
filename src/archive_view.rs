//! 이미지가 없는 압축 파일을 열었을 때의 화면과, 암호 입력 창, 압축 풀기 진행 표시.

use std::path::{Path, PathBuf};

use egui::text::{LayoutJob, TextWrapping};
use egui::{
    Align, Align2, Area, Button, Color32, CornerRadius, FontId, Frame, Id, Key, Layout, Modal,
    Painter, Pos2, ProgressBar, Rect, RichText, ScrollArea, Sense, TextEdit, Ui, UiBuilder,
    Vec2,
};

use crate::extract::{ExtractJob, Extracted, Format, Listing};
use crate::navigator::file_name;

const TEXT: Color32 = Color32::from_rgb(230, 232, 234);
const MUTED: Color32 = Color32::from_rgb(154, 163, 173);
const ACCENT: Color32 = Color32::from_rgb(240, 134, 90);
const ERROR: Color32 = Color32::from_rgb(255, 120, 120);
const LIST_BG: Color32 = Color32::from_rgb(36, 36, 40);
const FOLDER: Color32 = Color32::from_rgb(244, 196, 72);
const FOLDER_DARK: Color32 = Color32::from_rgb(214, 160, 44);
const ZIPPER: Color32 = Color32::from_rgb(70, 72, 80);

const COLUMN_WIDTH: f32 = 640.0;
const ICON_SIZE: f32 = 72.0;
const ROW_HEIGHT: f32 = 22.0;
const LIST_PADDING: f32 = 6.0;
/// 아이콘, 파일 이름, 요약 줄이 차지하는 높이.
const HEADER_HEIGHT: f32 = 156.0;
/// 단추 줄과 안내 문구가 차지하는 높이.
const FOOTER_HEIGHT: f32 = 90.0;
const SIZE_COLUMN: f32 = 90.0;
const OVERLAY_WIDTH: f32 = 420.0;

pub enum PanelAction {
    /// 압축 파일이 있는 폴더에 풉니다.
    Extract,
    /// 풀 폴더를 고릅니다.
    ExtractTo,
}

/// 갤러리로 보여 줄 이미지가 없는 압축 파일의 화면.
pub struct ArchivePanel {
    pub path: PathBuf,
    pub format: Format,
    /// gz 처럼 풀어 봐야 내용을 알 수 있거나, 암호가 없어 아직 읽지 못했으면 `None`.
    pub listing: Option<Listing>,
    /// 목록까지 암호화되어 있어 암호를 넣어야 내용이 보입니다.
    pub locked: bool,
}

impl ArchivePanel {
    pub fn show(&self, ui: &mut Ui, korean: bool, busy: bool) -> Option<PanelAction> {
        let t = |ko: &'static str, en: &'static str| if korean { ko } else { en };
        let view = ui.max_rect();
        let width = COLUMN_WIDTH.min(view.width() - 32.0).max(120.0);
        // 목록은 내용만큼만 차지하고, 남는 높이가 모자라면 스크롤합니다. 전체는 창 가운데에 둡니다.
        let rows = self.listing.as_ref().map_or(0, |listing| listing.entries.len());
        let room = (view.height() - 48.0 - HEADER_HEIGHT - FOOTER_HEIGHT).max(0.0);
        let wanted = (rows as f32 * ROW_HEIGHT + LIST_PADDING * 2.0).min(room);
        let list_height = if rows > 0 && wanted >= ROW_HEIGHT * 2.0 { wanted } else { 0.0 };
        let column = Rect::from_center_size(
            view.center(),
            Vec2::new(width, HEADER_HEIGHT + list_height + FOOTER_HEIGHT),
        );
        let mut action = None;

        ui.scope_builder(UiBuilder::new().max_rect(column), |ui| {
            ui.vertical_centered(|ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                let (icon, _) = ui.allocate_exact_size(Vec2::splat(ICON_SIZE), Sense::hover());
                draw_archive_icon(ui.painter(), icon);
                ui.label(RichText::new(file_name(&self.path)).size(18.0).color(TEXT));
                ui.label(RichText::new(self.summary(korean)).size(13.0).color(MUTED));
                ui.add_space(4.0);

                if let Some(listing) = self.listing.as_ref().filter(|_| list_height > 0.0) {
                    self.show_list(ui, listing, list_height);
                }
                ui.add_space(6.0);

                ui.allocate_ui_with_layout(
                    Vec2::new(width, 34.0),
                    Layout::left_to_right(Align::Center).with_main_align(Align::Center).with_main_justify(false),
                    |ui| {
                        // 가운데로 모으기 위해 두 단추의 폭만큼만 남기고 앞을 비웁니다.
                        let buttons = 150.0 + 190.0 + ui.spacing().item_spacing.x;
                        ui.add_space(((ui.available_width() - buttons) / 2.0).max(0.0));
                        let extract = Button::new(RichText::new(t("압축 풀기", "Extract")).size(15.0).color(Color32::WHITE))
                            .fill(ACCENT.gamma_multiply(0.85));
                        if ui.add_enabled(!busy, extract.min_size(Vec2::new(150.0, 32.0))).clicked() {
                            action = Some(PanelAction::Extract);
                        }
                        let elsewhere = Button::new(RichText::new(t("다른 폴더에 풀기...", "Extract to...")).size(15.0));
                        if ui.add_enabled(!busy, elsewhere.min_size(Vec2::new(190.0, 32.0))).clicked() {
                            action = Some(PanelAction::ExtractTo);
                        }
                    },
                );
                let hint = t(
                    "압축 풀기 (Ctrl+E) 는 이 압축 파일이 있는 폴더에 풉니다",
                    "Extract (Ctrl+E) unpacks next to the archive",
                );
                ui.label(RichText::new(hint).size(12.0).color(MUTED));
            });
        });
        action
    }

    pub fn summary(&self, korean: bool) -> String {
        let mut parts = vec![if korean {
            format!("{} 압축 파일", self.format.label())
        } else {
            format!("{} archive", self.format.label())
        }];
        match &self.listing {
            Some(listing) => {
                parts.push(if korean {
                    format!("파일 {}개", listing.files)
                } else {
                    format!("{} files", listing.files)
                });
                parts.push(format_size(listing.total_bytes));
                if listing.encrypted {
                    parts.push(if korean { "암호 있음" } else { "password protected" }.to_owned());
                }
            }
            None if self.locked => {
                parts.push(if korean { "암호를 넣어야 내용을 볼 수 있습니다" } else { "password needed to list" }.to_owned());
            }
            None => {
                if let Ok(meta) = std::fs::metadata(&self.path) {
                    parts.push(format_size(meta.len()));
                }
            }
        }
        parts.join("  ·  ")
    }

    fn show_list(&self, ui: &mut Ui, listing: &Listing, height: f32) {
        Frame::new()
            .fill(LIST_BG)
            .corner_radius(CornerRadius::same(6))
            .inner_margin(LIST_PADDING)
            .show(ui, |ui| {
                ui.set_height(height - LIST_PADDING * 2.0);
                ui.spacing_mut().item_spacing.y = 0.0;
                ScrollArea::vertical()
                    .id_salt(&self.path)
                    .auto_shrink(false)
                    .show_rows(ui, ROW_HEIGHT, listing.entries.len(), |ui, rows| {
                        for entry in &listing.entries[rows] {
                            let (row, _) = ui.allocate_exact_size(
                                Vec2::new(ui.available_width(), ROW_HEIGHT),
                                Sense::hover(),
                            );
                            let painter = ui.painter();
                            let size = if entry.is_dir { String::new() } else { format_size(entry.size) };
                            painter.text(
                                Pos2::new(row.right() - 6.0, row.center().y),
                                Align2::RIGHT_CENTER,
                                size,
                                FontId::proportional(12.0),
                                MUTED,
                            );
                            let color = if entry.is_dir { MUTED } else { TEXT };
                            let mut job = LayoutJob::simple_singleline(
                                entry.name.clone(),
                                FontId::proportional(13.0),
                                color,
                            );
                            job.wrap = TextWrapping::truncate_at_width(row.width() - SIZE_COLUMN - 12.0);
                            let galley = painter.layout_job(job);
                            let position = Pos2::new(row.left() + 6.0, row.center().y - galley.size().y / 2.0);
                            painter.galley(position, galley, color);
                        }
                    });
            });
    }
}

/// 지퍼가 달린 폴더 모양. Windows 의 압축 폴더 아이콘을 닮게 그립니다.
fn draw_archive_icon(painter: &Painter, rect: Rect) {
    let unit = rect.width() / 16.0;
    let at = |x: f32, y: f32| rect.min + Vec2::new(x * unit, y * unit);
    let radius = CornerRadius::same((unit * 0.8).round() as u8);
    // 뒤쪽 탭과 몸통.
    painter.rect_filled(Rect::from_min_max(at(1.0, 2.5), at(7.0, 5.0)), radius, FOLDER_DARK);
    painter.rect_filled(Rect::from_min_max(at(1.0, 4.0), at(15.0, 14.0)), radius, FOLDER);
    // 지퍼의 이와 손잡이.
    for step in 0..6 {
        let y = 4.6 + step as f32 * 1.1;
        let x = if step % 2 == 0 { 7.1 } else { 8.1 };
        painter.rect_filled(Rect::from_min_max(at(x, y), at(x + 0.9, y + 0.7)), CornerRadius::ZERO, ZIPPER);
    }
    let pull = Rect::from_min_max(at(7.2, 11.3), at(8.9, 13.2));
    painter.rect_filled(pull, CornerRadius::same((unit * 0.4).round() as u8), ZIPPER);
    painter.rect_filled(pull.shrink(unit * 0.45), CornerRadius::ZERO, FOLDER);
}

pub enum PromptOutcome {
    Pending,
    Submitted(String),
    Cancelled,
}

/// 암호를 묻는 창.
pub struct PasswordPrompt {
    file_name: String,
    input: String,
    reveal: bool,
    /// 앞서 넣은 암호가 틀렸습니다.
    wrong: bool,
    /// 창이 뜬 첫 프레임에 입력 칸으로 포커스를 옮깁니다.
    focus: bool,
}

impl PasswordPrompt {
    pub fn new(archive: &Path, wrong: bool) -> Self {
        Self { file_name: file_name(archive), input: String::new(), reveal: false, wrong, focus: true }
    }

    /// 앞서 넣은 암호가 틀려서 다시 묻는 중인지.
    #[cfg(test)]
    pub fn is_retry(&self) -> bool {
        self.wrong
    }

    pub fn show(&mut self, ctx: &egui::Context, korean: bool) -> PromptOutcome {
        let t = |ko: &'static str, en: &'static str| if korean { ko } else { en };
        let mut outcome = PromptOutcome::Pending;
        let frame = Frame::popup(&ctx.global_style()).inner_margin(16.0);
        let modal = Modal::new(Id::new("archive-password")).frame(frame).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.label(RichText::new(t("암호 입력", "Password")).size(17.0).color(TEXT));
            ui.label(RichText::new(&self.file_name).size(13.0).color(MUTED));
            if self.wrong {
                let message = t("암호가 올바르지 않습니다. 다시 입력하세요.", "Wrong password. Try again.");
                ui.label(RichText::new(message).size(13.0).color(ERROR));
                // 암호를 미리 검사하지 않는 형식은 손상된 파일과 틀린 암호를 구분하지 못합니다.
                let hint = t(
                    "맞는 암호인데도 계속 나오면 파일이 손상된 것일 수 있습니다.",
                    "If the password is right, the archive may be damaged.",
                );
                ui.label(RichText::new(hint).size(11.5).color(MUTED));
            } else {
                let message = t("암호로 보호된 압축 파일입니다.", "This archive is password protected.");
                ui.label(RichText::new(message).size(13.0).color(TEXT));
            }

            let field = ui.add(
                TextEdit::singleline(&mut self.input)
                    .password(!self.reveal)
                    .desired_width(f32::INFINITY)
                    .font(FontId::proportional(15.0)),
            );
            if std::mem::take(&mut self.focus) {
                field.request_focus();
            }
            // 한 줄 입력 칸은 Enter 를 누르면 포커스를 잃습니다.
            let entered = field.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
            ui.checkbox(&mut self.reveal, t("암호 표시", "Show password"));

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let size = Vec2::new(72.0, 28.0);
                if ui.add(Button::new(t("취소", "Cancel")).min_size(size)).clicked() {
                    outcome = PromptOutcome::Cancelled;
                }
                let confirm =
                    ui.add_enabled(!self.input.is_empty(), Button::new(t("확인", "OK")).min_size(size));
                if (confirm.clicked() || entered) && !self.input.is_empty() {
                    outcome = PromptOutcome::Submitted(self.input.clone());
                } else if entered {
                    self.focus = true;
                }
            });
        });
        if matches!(outcome, PromptOutcome::Pending) && modal.should_close() {
            outcome = PromptOutcome::Cancelled;
        }
        outcome
    }
}

pub enum OverlayAction {
    Cancel,
    /// 풀어 놓은 폴더를 탐색기로 엽니다.
    Reveal,
    Dismiss,
}

/// 화면 아래쪽에 띄우는 압축 풀기 상태.
pub enum ExtractState {
    Idle,
    Running(ExtractJob),
    Done(Extracted),
    Failed(String),
}

impl ExtractState {
    pub fn is_running(&self) -> bool {
        matches!(self, ExtractState::Running(_))
    }

    pub fn show(&self, ctx: &egui::Context, korean: bool) -> Option<OverlayAction> {
        let t = |ko: &'static str, en: &'static str| if korean { ko } else { en };
        if matches!(self, ExtractState::Idle) {
            return None;
        }
        let mut action = None;
        Area::new(Id::new("extract-overlay"))
            .anchor(Align2::CENTER_BOTTOM, [0.0, -20.0])
            .show(ctx, |ui| {
                Frame::popup(ui.style()).inner_margin(12.0).show(ui, |ui| {
                    ui.set_width(OVERLAY_WIDTH);
                    ui.spacing_mut().item_spacing.y = 6.0;
                    match self {
                        ExtractState::Idle => {}
                        ExtractState::Running(job) => {
                            let title = format!("{}  {}", t("압축 푸는 중", "Extracting"), file_name(&job.archive));
                            ui.add(egui::Label::new(RichText::new(title).size(14.0).color(TEXT)).truncate());
                            let bar = match job.progress.fraction() {
                                Some(fraction) => ProgressBar::new(fraction).show_percentage(),
                                None => ProgressBar::new(0.0).animate(true),
                            };
                            ui.add(bar.desired_width(OVERLAY_WIDTH).fill(ACCENT.gamma_multiply(0.85)));
                            ui.horizontal(|ui| {
                                if ui.button(t("취소", "Cancel")).clicked() {
                                    action = Some(OverlayAction::Cancel);
                                }
                                let current = RichText::new(job.progress.current()).size(12.0).color(MUTED);
                                ui.add(egui::Label::new(current).truncate());
                            });
                        }
                        ExtractState::Done(extracted) => {
                            let title = if korean {
                                format!("압축을 풀었습니다  ·  파일 {}개", extracted.files)
                            } else {
                                format!("Extracted  ·  {} files", extracted.files)
                            };
                            ui.label(RichText::new(title).size(14.0).color(TEXT));
                            let target = RichText::new(extracted.target.display().to_string()).size(12.0).color(MUTED);
                            ui.add(egui::Label::new(target).truncate());
                            if extracted.skipped > 0 {
                                let note = if korean {
                                    format!("링크·장치 파일 {}개는 만들지 않고 건너뛰었습니다", extracted.skipped)
                                } else {
                                    format!("Skipped {} links or device files", extracted.skipped)
                                };
                                ui.label(RichText::new(note).size(12.0).color(ACCENT));
                            }
                            ui.horizontal(|ui| {
                                let reveal = if extracted.target.is_dir() {
                                    t("폴더 열기", "Open folder")
                                } else {
                                    t("폴더에서 보기", "Show in folder")
                                };
                                if ui.button(reveal).clicked() {
                                    action = Some(OverlayAction::Reveal);
                                }
                                if ui.button(t("닫기", "Close")).clicked() {
                                    action = Some(OverlayAction::Dismiss);
                                }
                            });
                        }
                        ExtractState::Failed(message) => {
                            let title = t("압축을 풀지 못했습니다", "Extraction failed");
                            ui.label(RichText::new(title).size(14.0).color(ERROR));
                            ui.label(RichText::new(message).size(12.0).color(TEXT));
                            if ui.button(t("닫기", "Close")).clicked() {
                                action = Some(OverlayAction::Dismiss);
                            }
                        }
                    }
                });
            });
        action
    }
}

/// 탐색기에서 `target`을 보여 줍니다. 폴더는 열고, 파일은 선택된 채로 보여 줍니다.
pub fn reveal_in_explorer(target: &Path) -> std::io::Result<()> {
    let mut command = std::process::Command::new("explorer.exe");
    if target.is_dir() {
        command.arg(target);
    } else {
        select_argument(&mut command, target);
    }
    command.spawn().map(|_| ())
}

#[cfg(windows)]
fn select_argument(command: &mut std::process::Command, target: &Path) {
    use std::os::windows::process::CommandExt as _;
    // 탐색기는 `/select,"경로"` 꼴만 알아듣습니다. 인자 전체를 따옴표로 싸면 안 됩니다.
    command.raw_arg(format!("/select,\"{}\"", target.display()));
}

#[cfg(not(windows))]
fn select_argument(command: &mut std::process::Command, target: &Path) {
    command.arg(target.parent().unwrap_or(target));
}

pub fn format_size(bytes: u64) -> String {
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
