//! Finding faces on whatever machine this runs on.
//!
//! Three roles: body text, a heavier face for the big numbers, and a monospace
//! face for hashes, where columns of hex have to line up to be read. Each falls
//! back to the one before it, and body falls back to Denise's built-in bitmap
//! font, so a panel with no fonts installed still starts.

use std::path::{Path, PathBuf};

use denise_text::{GlyphSource, TrueTypeSource};

const FONT_DIRS: &[&str] = &[
    "/usr/share/fonts",
    "/usr/local/share/fonts",
    "/System/Library/Fonts",
    "/System/Library/Fonts/Supplemental",
    "/Library/Fonts",
    "C:\\Windows\\Fonts",
];

/// In order of preference; matched case-insensitively against file names.
const REGULAR: &[&str] = &[
    "Inter-Regular.ttf",
    "InterVariable.ttf",
    "segoeui.ttf",
    "NotoSans-Regular.ttf",
    "Ubuntu-R.ttf",
    "Cantarell-Regular.otf",
    "DejaVuSans.ttf",
    "LiberationSans-Regular.ttf",
    "HelveticaNeue.ttc",
    "Arial.ttf",
    "Helvetica.ttc",
];

const BOLD: &[&str] = &[
    "Inter-SemiBold.ttf",
    "Inter-Bold.ttf",
    "segoeuib.ttf",
    "seguisb.ttf",
    "NotoSans-SemiBold.ttf",
    "NotoSans-Bold.ttf",
    "Ubuntu-B.ttf",
    "DejaVuSans-Bold.ttf",
    "LiberationSans-Bold.ttf",
    "Arial Bold.ttf",
    "arialbd.ttf",
];

const MONO: &[&str] = &[
    "JetBrainsMono-Regular.ttf",
    "CascadiaMono.ttf",
    "consola.ttf",
    "SFNSMono.ttf",
    "Menlo.ttc",
    "NotoSansMono-Regular.ttf",
    "UbuntuMono-R.ttf",
    "DejaVuSansMono.ttf",
    "LiberationMono-Regular.ttf",
    "Courier New.ttf",
    "cour.ttf",
];

pub struct Faces {
    pub regular: Option<Loaded>,
    pub bold: Option<Loaded>,
    pub mono: Option<Loaded>,
}

pub struct Loaded {
    pub name: String,
    pub source: Box<dyn GlyphSource>,
}

/// Searches the usual directories once and loads the best of each role.
///
/// `HANSOLO_FONT`, `HANSOLO_FONT_BOLD` and `HANSOLO_FONT_MONO` override the
/// search with a path, which is how a panel image names the face it ships.
pub fn load() -> Faces {
    let mut found = Vec::new();
    for dir in FONT_DIRS {
        collect(Path::new(dir), 0, &mut found);
    }
    let pick = |env: &str, wanted: &[&str]| -> Option<Loaded> {
        let path = std::env::var_os(env)
            .map(PathBuf::from)
            .or_else(|| preferred(&found, wanted).cloned())?;
        open(&path)
    };
    Faces {
        regular: pick("HANSOLO_FONT", REGULAR),
        bold: pick("HANSOLO_FONT_BOLD", BOLD),
        mono: pick("HANSOLO_FONT_MONO", MONO),
    }
}

fn open(path: &Path) -> Option<Loaded> {
    let name = path.display().to_string();
    let bytes = std::fs::read(path).ok()?;
    match TrueTypeSource::from_bytes(&name, &bytes) {
        Ok(source) => Some(Loaded {
            name,
            source: Box::new(source),
        }),
        Err(why) => {
            eprintln!("font {name}: {why}");
            None
        }
    }
}

fn collect(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 3 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, depth + 1, out);
        } else if matches!(
            path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref(),
            Some("ttf" | "otf" | "ttc")
        ) {
            out.push(path);
        }
    }
}

fn preferred<'a>(found: &'a [PathBuf], wanted: &[&str]) -> Option<&'a PathBuf> {
    wanted.iter().find_map(|name| {
        found.iter().find(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
        })
    })
}
