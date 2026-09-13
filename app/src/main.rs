//! HanSolo: a Bitcoin solo lottery miner with a DeniseUI dashboard.
//!
//! ```text
//! hansolo                       # a window, or the display itself on bare Linux
//! hansolo --ui kiosk            # take over the display (DRM/KMS, fbdev)
//! hansolo --ui headless         # no display at all; status lines on stderr
//! hansolo --demo                # the dashboard with a synthetic miner
//! hansolo --snapshot out.ppm    # draw one frame into a file and exit
//! ```
//!
//! # Which display
//!
//! Denise leaves the backend to the application, at compile time, because a
//! library cannot know whether an `aarch64-unknown-linux-gnu` binary is on a
//! kiosk or a desktop. This application *does* know once it is running: a
//! session with `WAYLAND_DISPLAY` or `DISPLAY` has a compositor to ask for a
//! window, and one without, but with `/dev/dri`, has a display to take. So a
//! default Linux build compiles both backends and `--ui auto` picks by that
//! rule; `--ui` overrides it either way.

mod app;
mod demo;
#[cfg(feature = "desktop")]
mod desktop;
mod fonts;
mod format;
mod headless;
#[cfg(all(feature = "kiosk", target_os = "linux"))]
mod kiosk;
mod refresh;
mod store;
mod theme;
mod view;
mod widgets;

use std::path::PathBuf;

use denise::Size;
use hansolo_engine::Miner;

use crate::app::{App, LocalClipboard, Setup};
use crate::store::Store;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    Window,
    Kiosk,
    Headless,
}

struct Args {
    ui: Option<Backend>,
    config: Option<PathBuf>,
    demo: bool,
    snapshot: Option<Snapshot>,
}

struct Snapshot {
    path: String,
    size: Size,
    scale: f32,
    page: usize,
    theme: hansolo_core::ThemePreference,
}

const HELP: &str = "\
HanSolo — Bitcoin solo lottery miner

USAGE:
    hansolo [OPTIONS]

OPTIONS:
    --ui <auto|window|kiosk|headless>  Where to show the dashboard [default: auto]
    --config <FILE>                   Configuration file [default: platform config dir]
    --demo                            Show a synthetic miner instead of mining
    --snapshot <FILE.ppm>             Draw one frame into a file and exit
        --size <WxH>                  Snapshot size [default: 1320x900]
        --scale <F>                   Snapshot scale factor [default: 1]
        --page <N>                    Snapshot page 0-5 [default: 0]
        --theme <dark|light>          Snapshot theme [default: dark]
    -h, --help                        This text

KEYS:
    F5 starts or stops mining. On a bare display, F12 writes /tmp/hansolo.ppm
    and Escape quits.
";

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        ui: None,
        config: None,
        demo: false,
        snapshot: None,
    };
    let mut snapshot: Option<Snapshot> = None;
    let mut it = std::env::args().skip(1);
    let value = |it: &mut std::iter::Skip<std::env::Args>, flag: &str| {
        it.next().ok_or_else(|| format!("{flag} needs a value"))
    };
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                std::process::exit(0);
            }
            "--ui" => {
                args.ui = match value(&mut it, "--ui")?.as_str() {
                    "auto" => None,
                    "window" => Some(Backend::Window),
                    "kiosk" => Some(Backend::Kiosk),
                    "headless" => Some(Backend::Headless),
                    other => return Err(format!("unknown --ui {other}")),
                }
            }
            "--config" => args.config = Some(value(&mut it, "--config")?.into()),
            "--demo" => args.demo = true,
            "--snapshot" => {
                snapshot = Some(Snapshot {
                    path: value(&mut it, "--snapshot")?,
                    size: Size::new(1320, 900),
                    scale: 1.0,
                    page: 0,
                    theme: hansolo_core::ThemePreference::Dark,
                })
            }
            "--size" | "--scale" | "--page" | "--theme" => {
                let v = value(&mut it, &arg)?;
                let s = snapshot
                    .as_mut()
                    .ok_or_else(|| format!("{arg} only applies to --snapshot"))?;
                match arg.as_str() {
                    "--size" => {
                        let (w, h) = v.split_once('x').ok_or("--size is WxH")?;
                        s.size = Size::new(
                            w.parse().map_err(|_| "bad width")?,
                            h.parse().map_err(|_| "bad height")?,
                        );
                    }
                    "--scale" => s.scale = v.parse().map_err(|_| "bad scale")?,
                    "--page" => s.page = v.parse().map_err(|_| "bad page")?,
                    _ => {
                        s.theme = match v.as_str() {
                            "light" => hansolo_core::ThemePreference::Light,
                            _ => hansolo_core::ThemePreference::Dark,
                        }
                    }
                }
            }
            other => return Err(format!("unknown argument {other}; see --help")),
        }
    }
    args.snapshot = snapshot;
    Ok(args)
}

fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(e) => {
            eprintln!("hansolo: {e}");
            std::process::exit(2);
        }
    };
    if let Err(e) = run(args) {
        eprintln!("hansolo: {e}");
        std::process::exit(1);
    }
}

fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    theme::validate();
    let store = Store::new(args.config.clone());
    let mut config = store.load_config();
    let miner = Miner::new();

    if let Some(shot) = args.snapshot {
        config.ui.theme = shot.theme;
        let setup = Setup {
            store,
            config,
            miner,
            faces: fonts::load(),
            windowed: false,
            clipboard: Box::new(LocalClipboard::default()),
            demo: true,
        };
        return snapshot(setup, &shot);
    }

    let backend = args.ui.unwrap_or_else(auto_backend);
    eprintln!(
        "hansolo {} · ui {backend:?} · config {}",
        env!("CARGO_PKG_VERSION"),
        store.config_path().display()
    );
    if backend == Backend::Headless {
        return headless::run(miner, config, &store);
    }

    let windowed = backend == Backend::Window;
    let setup = Setup {
        store,
        config,
        miner,
        faces: fonts::load(),
        windowed,
        clipboard: new_clipboard(windowed),
        demo: args.demo,
    };
    match backend {
        #[cfg(feature = "desktop")]
        Backend::Window => desktop::run(setup),
        #[cfg(all(feature = "kiosk", target_os = "linux"))]
        Backend::Kiosk => kiosk::run(setup),
        other => Err(format!(
            "this build has no {other:?} backend; rebuild with its feature, or use --ui headless"
        )
        .into()),
    }
}

/// See the module docs for the rule.
fn auto_backend() -> Backend {
    let has_session = ["WAYLAND_DISPLAY", "DISPLAY"]
        .iter()
        .any(|v| std::env::var_os(v).is_some_and(|s| !s.is_empty()));
    if (cfg!(not(target_os = "linux")) || has_session) && cfg!(feature = "desktop") {
        return Backend::Window;
    }
    #[cfg(all(feature = "kiosk", target_os = "linux"))]
    if kiosk::available() {
        return Backend::Kiosk;
    }
    if cfg!(feature = "desktop") && cfg!(not(target_os = "linux")) {
        Backend::Window
    } else {
        Backend::Headless
    }
}

fn new_clipboard(windowed: bool) -> Box<dyn app::Clipboard> {
    #[cfg(feature = "desktop")]
    if windowed {
        return Box::new(desktop::SystemClipboard::default());
    }
    let _ = windowed;
    Box::new(LocalClipboard::default())
}

/// Draws one frame into a PPM, with no display. How the screenshots in the
/// README are made, and how a layout is reviewed over SSH.
fn snapshot(setup: Setup, shot: &Snapshot) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write as _;

    let size = Size::new(
        (shot.size.width as f32 * shot.scale) as u32,
        (shot.size.height as f32 * shot.scale) as u32,
    );
    let mut app = App::new(setup, size, shot.scale);
    app.ui.show_cursor(false);
    app.ui.clear_toasts();
    app.update(&[]);
    if shot.page != 0 {
        app.update(&[]);
    }
    app.show_page(shot.page);
    // Toasts fade in; land them, then let the tree settle.
    for step in 1..=5 {
        app.ui.tick(app.elapsed_ms() + step * 2000);
    }

    let mut pixels = vec![0u32; (size.width * size.height) as usize];
    {
        let mut frame = denise::Frame::new(
            &mut pixels,
            size,
            size.width,
            denise::PixelFormat::Xrgb8888,
            denise::BufferAge::Undefined,
        )?;
        app.ui.paint(&mut frame);
    }
    let mut out = std::io::BufWriter::new(std::fs::File::create(&shot.path)?);
    write!(out, "P6\n{} {}\n255\n", size.width, size.height)?;
    for word in &pixels {
        out.write_all(&[(word >> 16) as u8, (word >> 8) as u8, *word as u8])?;
    }
    out.flush()?;
    eprintln!("wrote {} at {}x{}", shot.path, size.width, size.height);
    Ok(())
}
