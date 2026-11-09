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
    Fit,
    ActualSize,
    ZoomIn,
    ZoomOut,
    /// Wheel zoom; the factor already accounts for the number of notches.
    ZoomBy(f32),
    ToggleFullscreen,
    /// Leave fullscreen, or quit when not fullscreen.
    Escape,
    /// Re-scan the folder.
    Refresh,
    ToggleInfo,
}

/// Collects all actions triggered by this frame's input.
pub fn collect_actions(input: &InputState) -> Vec<Action> {
    let mut actions = Vec::new();
    let modifiers = input.modifiers;
    let pressed = |key: Key| input.key_pressed(key);

    if modifiers.command && pressed(Key::O) {
        actions.push(Action::Open);
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

    // Mouse wheel: one notch = one zoom step, anchored at the cursor.
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
        actions.push(Action::ZoomBy(ZOOM_STEP.powf(notches)));
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
