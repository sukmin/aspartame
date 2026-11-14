//! Maps raw keyboard, wheel and drag-and-drop input to viewer actions.

use std::path::PathBuf;

use egui::{Event, InputState, Key, MouseWheelUnit};

use crate::viewport::ZOOM_STEP;

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Show the native file picker.
    Open,
    /// Open the first existing file of a drag-and-drop.
    OpenPaths(Vec<PathBuf>),
    Next,
    Prev,
    First,
    Last,
    /// 갤러리에서 한 줄 위로.
    Up,
    /// 갤러리에서 한 줄 아래로.
    Down,
    /// 갤러리에서 선택한 이미지 열기.
    Activate,
    /// 현재 CR3의 내장 JPEG를 파일로 저장.
    SaveJpeg,
    /// 열어 둔 압축 파일을 그 파일이 있는 폴더에 풀기.
    Extract,
    /// 열어 둔 압축 파일을 고른 폴더에 풀기.
    ExtractTo,
    Fit,
    ActualSize,
    ZoomIn,
    ZoomOut,
    /// Wheel zoom; the factor already accounts for the number of notches.
    ZoomBy(f32),
    /// Alt 없이 휠을 굴린 양(칸 수). 위로 굴리면 양수입니다.
    Wheel(f32),
    ToggleFullscreen,
    /// Leave fullscreen, or quit when not fullscreen.
    Escape,
    /// Re-scan the folder.
    Refresh,
    ToggleInfo,
}

/// 휠 입력을 온전한 칸 수로 바꿉니다. 터치패드와 고해상도 휠은 한 칸보다 작은 값을
/// 연달아 보내므로, 모아 두지 않으면 프레임마다 이미지가 넘어갑니다.
#[derive(Default)]
pub struct WheelStepper {
    accumulated: f32,
}

impl WheelStepper {
    pub fn feed(&mut self, notches: f32) -> isize {
        // 방향을 바꾸면 반대쪽으로 모아 둔 값은 버립니다.
        if self.accumulated * notches < 0.0 {
            self.accumulated = 0.0;
        }
        self.accumulated += notches;
        let steps = self.accumulated.trunc();
        self.accumulated -= steps;
        steps as isize
    }
}

/// Collects all actions triggered by this frame's input.
pub fn collect_actions(input: &InputState) -> Vec<Action> {
    let mut actions = Vec::new();
    let modifiers = input.modifiers;
    let pressed = |key: Key| input.key_pressed(key);

    if modifiers.command && pressed(Key::O) {
        actions.push(Action::Open);
    }
    if modifiers.command && pressed(Key::S) {
        actions.push(Action::SaveJpeg);
    }
    if modifiers.command && pressed(Key::E) {
        actions.push(if modifiers.shift { Action::ExtractTo } else { Action::Extract });
    }
    if pressed(Key::ArrowUp) {
        actions.push(Action::Up);
    }
    if pressed(Key::ArrowDown) {
        actions.push(Action::Down);
    }
    if pressed(Key::Enter) {
        actions.push(Action::Activate);
    }

    if pressed(Key::ArrowRight) || pressed(Key::PageDown) {
        actions.push(Action::Next);
    }
    if pressed(Key::ArrowLeft) || pressed(Key::PageUp) {
        actions.push(Action::Prev);
    }
    if pressed(Key::Home) {
        actions.push(Action::First);
    }
    if pressed(Key::End) {
        actions.push(Action::Last);
    }

    if pressed(Key::F) || pressed(Key::Num0) {
        actions.push(Action::Fit);
    }
    if pressed(Key::Num1) {
        actions.push(Action::ActualSize);
    }
    if pressed(Key::Plus) || pressed(Key::Equals) {
        actions.push(Action::ZoomIn);
    }
    if pressed(Key::Minus) {
        actions.push(Action::ZoomOut);
    }

    if pressed(Key::F11) {
        actions.push(Action::ToggleFullscreen);
    }
    if pressed(Key::Escape) {
        actions.push(Action::Escape);
    }
    if pressed(Key::F5) {
        actions.push(Action::Refresh);
    }
    if pressed(Key::I) {
        actions.push(Action::ToggleInfo);
    }

    // 휠은 이전/다음 이미지, Alt+휠은 커서 기준 확대/축소입니다.
    let mut notches = 0.0f32;
    for event in &input.events {
        if let Event::MouseWheel { unit, delta, .. } = event {
            notches += match unit {
                MouseWheelUnit::Line => delta.y,
                MouseWheelUnit::Point => delta.y / 50.0,
                MouseWheelUnit::Page => delta.y * 5.0,
            };
        }
    }
    if notches != 0.0 {
        actions.push(if modifiers.alt {
            Action::ZoomBy(ZOOM_STEP.powf(notches))
        } else {
            Action::Wheel(notches)
        });
    }

    let dropped: Vec<PathBuf> = input
        .raw
        .dropped_files
        .iter()
        .map(|file| file.path().to_path_buf())
        .collect();
    if !dropped.is_empty() {
        actions.push(Action::OpenPaths(dropped));
    }

    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Modifiers, RawInput, TouchPhase, Vec2};

    fn actions_for_wheel(delta_y: f32, modifiers: Modifiers) -> Vec<Action> {
        let ctx = egui::Context::default();
        let mut raw = RawInput::default();
        raw.events.push(Event::ModifiersChanged(modifiers));
        raw.events.push(Event::MouseWheel {
            unit: MouseWheelUnit::Line,
            delta: Vec2::new(0.0, delta_y),
            phase: TouchPhase::Move,
            modifiers,
        });
        ctx.begin_pass(raw);
        let actions = ctx.input(collect_actions);
        ctx.end_pass().textures_delta.clear();
        actions
    }

    #[test]
    fn plain_wheel_navigates_and_alt_wheel_zooms() {
        assert_eq!(actions_for_wheel(-1.0, Modifiers::NONE), vec![Action::Wheel(-1.0)]);
        assert_eq!(actions_for_wheel(1.0, Modifiers::ALT), vec![Action::ZoomBy(ZOOM_STEP)]);
    }

    #[test]
    fn small_wheel_deltas_add_up_to_one_step() {
        let mut wheel = WheelStepper::default();
        assert_eq!(wheel.feed(-0.4), 0);
        assert_eq!(wheel.feed(-0.4), 0);
        assert_eq!(wheel.feed(-0.4), -1);
        assert_eq!(wheel.feed(3.0), 3);
    }

    #[test]
    fn changing_wheel_direction_discards_the_remainder() {
        let mut wheel = WheelStepper::default();
        assert_eq!(wheel.feed(-0.9), 0);
        assert_eq!(wheel.feed(0.5), 0);
        assert_eq!(wheel.feed(0.5), 1);
    }
}
