//! The display itself, on a Linux machine with no desktop: DRM/KMS, or fbdev
//! where there is no `/dev/dri`, with evdev input and the console muted.
//!
//! Adapted from Denise's `bare-linux` example support crate, which is not
//! published. The loop stays here rather than in a helper, as Denise argues it
//! should: where input is read relative to the display wait is a decision with
//! a measurable cost.

use std::os::fd::{BorrowedFd, RawFd};
use denise::{ElementState, InputEvent, InputSource, KeyCode, PixelFormat, Rect, Size, Surface, SurfaceError};
use denise_drm::{DrmSurface, PresentMode, SurfaceConfig};
use denise_evdev::{Console, InputBackend};
use denise_fbdev::FbdevSurface;
use rustix::event::{PollFd, PollFlags, Timespec, poll};

use crate::app::{App, Setup};

const SHOT_PATH: &str = "/tmp/hansolo.ppm";

/// Whether this machine looks like it has a display to take over.
pub fn available() -> bool {
    std::fs::read_dir("/dev/dri")
        .map(|mut entries| entries.any(|e| e.is_ok_and(|e| e.file_name().to_string_lossy().starts_with("card"))))
        .unwrap_or(false)
        || std::path::Path::new("/dev/fb0").exists()
}

#[allow(clippy::large_enum_variant)]
enum Display {
    Drm(DrmSurface),
    Fbdev(FbdevSurface),
}

impl Display {
    fn open() -> Result<Self, String> {
        match DrmSurface::open(SurfaceConfig { present_mode: PresentMode::Vsync, ..SurfaceConfig::default() }) {
            Ok(drm) => {
                eprintln!("display DRM/KMS {} — {} buffers", drm.mode_name(), drm.buffer_count());
                Ok(Display::Drm(drm))
            }
            Err(drm_error) => match FbdevSurface::open_first() {
                Ok(fb) => {
                    eprintln!("display fbdev {} ({}); no DRM: {drm_error}", fb.info(), fb.path().display());
                    Ok(Display::Fbdev(fb))
                }
                Err(fb_error) => Err(format!("no display — DRM: {drm_error}; fbdev: {fb_error}")),
            },
        }
    }
}

impl Surface for Display {
    fn size(&self) -> Size {
        match self {
            Display::Drm(d) => d.size(),
            Display::Fbdev(f) => f.size(),
        }
    }
    fn scale_factor(&self) -> f32 {
        match self {
            Display::Drm(d) => d.scale_factor(),
            Display::Fbdev(f) => f.scale_factor(),
        }
    }
    fn format(&self) -> PixelFormat {
        match self {
            Display::Drm(d) => d.format(),
            Display::Fbdev(f) => f.format(),
        }
    }
    fn acquire(&mut self) -> Result<denise::Frame<'_>, SurfaceError> {
        match self {
            Display::Drm(d) => d.acquire(),
            Display::Fbdev(f) => f.acquire(),
        }
    }
    fn present(&mut self, damage: &[Rect]) -> Result<(), SurfaceError> {
        match self {
            Display::Drm(d) => d.present(damage),
            Display::Fbdev(f) => f.present(damage),
        }
    }
}

fn mute_console() -> Option<Console> {
    if std::env::var_os("DENISE_KEEP_CONSOLE").is_some() {
        return None;
    }
    let mut console = Console::open_if_present()?;
    if let Err(e) = console.mute_keyboard().and_then(|()| console.graphics_mode()) {
        eprintln!("console found but not muted: {e}");
    }
    Some(console)
}

/// A panel's density: 1× up to 1080p, then in proportion, so a 4K panel is not
/// a postage stamp. `ui.scale` in the config overrides it.
fn panel_scale(size: Size) -> f32 {
    (size.height as f32 / 1080.0).max(1.0)
}

pub fn run(setup: Setup) -> Result<(), Box<dyn std::error::Error>> {
    let mut surface = Display::open()?;
    let size = surface.size();
    let mut input = InputBackend::open_all(size)?;
    let (layout, source) = input.set_layout_from_system();
    eprintln!("keymap  {} (from {source})", layout.name);
    let _console = mute_console();

    let mut app = App::new(setup, size, panel_scale(size));
    eprintln!("\nF5 starts or stops mining, F12 writes {SHOT_PATH}, Escape quits\n");

    let mut fds: Vec<RawFd> = input.raw_fds();
    present(&mut surface, &mut app, false)?;

    let mut events = Vec::new();
    let mut shoot = false;
    loop {
        if input.devices_changed() {
            fds = input.raw_fds();
        }
        let wait = app.next_wake_in();
        let timeout = Timespec { tv_sec: wait.as_secs() as i64, tv_nsec: wait.subsec_nanos() as i64 };
        let mut poll_fds: Vec<PollFd<'_>> = fds
            .iter()
            // SAFETY: `input` keeps every one of these descriptors open until a
            // rescan, which sets `devices_changed` and refreshes `fds` above
            // before the next poll.
            .map(|&fd| PollFd::from_borrowed_fd(unsafe { BorrowedFd::borrow_raw(fd) }, PollFlags::IN))
            .collect();
        match poll(&mut poll_fds, Some(&timeout)) {
            Ok(_) | Err(rustix::io::Errno::INTR) => {}
            Err(e) => return Err(e.into()),
        }
        drop(poll_fds);

        events.clear();
        input.poll(&mut events);
        for event in &events {
            if let InputEvent::Key { code, state: ElementState::Down, .. } = event {
                match code {
                    KeyCode::Escape if app.wants_exit_on_escape() => {
                        app.miner.stop();
                        return Ok(());
                    }
                    KeyCode::F12 => shoot = true,
                    _ => {}
                }
            }
        }
        app.update(&events);
        if app.exit {
            app.miner.stop();
            return Ok(());
        }
        if app.ui.needs_paint() || shoot {
            present(&mut surface, &mut app, std::mem::take(&mut shoot))?;
        }
    }
}

fn present(surface: &mut Display, app: &mut App, shoot: bool) -> Result<(), Box<dyn std::error::Error>> {
    let mut frame = surface.acquire()?;
    app.ui.paint(&mut frame);
    if shoot {
        match capture(&frame) {
            Ok(()) => eprintln!("wrote {SHOT_PATH}"),
            Err(e) => eprintln!("could not write {SHOT_PATH}: {e}"),
        }
    }
    drop(frame);
    surface.present(app.ui.damage())?;
    app.ui.presented();
    Ok(())
}

/// A screenshot of a machine with no screenshot tool: the frame about to be shown.
fn capture(frame: &denise::Frame<'_>) -> std::io::Result<()> {
    use std::io::Write as _;
    let size = frame.size();
    let mut out = std::io::BufWriter::new(std::fs::File::create(SHOT_PATH)?);
    write!(out, "P6\n{} {}\n255\n", size.width, size.height)?;
    for y in 0..size.height {
        let Some(row) = frame.row(y) else { continue };
        for word in &row[..size.width as usize] {
            out.write_all(&[(word >> 16) as u8, (word >> 8) as u8, *word as u8])?;
        }
    }
    out.flush()
}
