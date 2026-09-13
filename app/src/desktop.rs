//! A window, on macOS, Windows or a Linux desktop.

use std::time::Duration;

use denise::{DamageTracker, ElementState, Frame, InputEvent, KeyCode, Rect, Size};
use denise_winit::{DeniseApp, WindowConfig, run_with};

use crate::app::{App, Clipboard, Setup};

pub fn run(setup: Setup) -> Result<(), Box<dyn std::error::Error>> {
    run_with(
        WindowConfig {
            title: "HanSolo — Bitcoin solo lottery miner".into(),
            size: Size::new(1320, 900),
            ..WindowConfig::default()
        },
        move |surface, scale| {
            let mut app = App::new(setup, surface, scale);
            // The window system draws the pointer; the tree must not draw a second.
            app.ui.show_cursor(false);
            Window(app)
        },
    )?;
    Ok(())
}

struct Window(App);

impl DeniseApp for Window {
    fn update(&mut self, events: &[InputEvent], damage: &mut DamageTracker) {
        for event in events {
            if let InputEvent::Key {
                code: KeyCode::Escape,
                state: ElementState::Down,
                ..
            } = event
                && self.0.wants_exit_on_escape()
            {
                // Escape clears focus first; it only ever quits from an idle window
                // on a kiosk, never here, where a close button exists.
            }
        }
        self.0.update(events);
        if self.0.ui.needs_paint() {
            let pending = self.0.ui.pending_damage();
            if pending.is_empty() {
                damage.add_full();
            } else {
                for rect in pending {
                    damage.add(*rect);
                }
            }
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, _damage: &[Rect]) {
        self.0.ui.paint(frame);
        self.0.ui.presented();
    }

    fn exit_requested(&self) -> bool {
        self.0.exit
    }

    fn next_frame_in(&self) -> Option<Duration> {
        Some(self.0.next_wake_in())
    }

    fn exiting(&mut self) {
        self.0.miner.stop();
    }
}

/// The system clipboard, through arboard. Opened lazily: some Linux sessions
/// have no clipboard owner, and failing there should cost a paste, not a start.
#[derive(Default)]
pub struct SystemClipboard(Option<arboard::Clipboard>);

impl SystemClipboard {
    fn handle(&mut self) -> Option<&mut arboard::Clipboard> {
        if self.0.is_none() {
            self.0 = arboard::Clipboard::new().ok();
        }
        self.0.as_mut()
    }
}

impl Clipboard for SystemClipboard {
    fn get(&mut self) -> Option<String> {
        self.handle()?.get_text().ok()
    }

    fn set(&mut self, text: &str) {
        if let Some(clipboard) = self.handle() {
            let _ = clipboard.set_text(text.to_string());
        }
    }
}
