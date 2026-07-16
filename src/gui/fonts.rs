//! Load system fonts so Japanese and Thai render correctly in the native GUI.

use std::path::Path;
use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily};

/// Prefer these paths (first match wins per slot). Covers Linux / macOS / Windows.
const SANS_CANDIDATES: &[&str] = &[
    // Linux
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/Adwaita/AdwaitaSans-Regular.ttf",
    "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
    // macOS
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "/Library/Fonts/Arial Unicode.ttf",
    // Windows
    "C:\\Windows\\Fonts\\segoeui.ttf",
    "C:\\Windows\\Fonts\\meiryo.ttc",
    "C:\\Windows\\Fonts\\msyh.ttc",
];

const CJK_CANDIDATES: &[&str] = &[
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/noto-cjk/NotoSansCJKjp-Regular.otf",
    "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "C:\\Windows\\Fonts\\msgothic.ttc",
    "C:\\Windows\\Fonts\\meiryo.ttc",
    "C:\\Windows\\Fonts\\msyh.ttc",
    "C:\\Windows\\Fonts\\YuGothM.ttc",
];

const THAI_CANDIDATES: &[&str] = &[
    "/usr/share/fonts/noto/NotoSansThai-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansThai-Regular.ttf",
    "/System/Library/Fonts/Supplemental/Thonburi.ttc",
    "C:\\Windows\\Fonts\\leelawad.ttf",
    "C:\\Windows\\Fonts\\LeelawadeeUI.ttf",
];

const MONO_CANDIDATES: &[&str] = &[
    "/usr/share/fonts/Adwaita/AdwaitaMono-Regular.ttf",
    "/usr/share/fonts/noto/NotoSansMono-Regular.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/System/Library/Fonts/Menlo.ttc",
    "C:\\Windows\\Fonts\\consola.ttf",
    "C:\\Windows\\Fonts\\cascadiamono.ttf",
];

pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();

    if let Some(data) = load_first(SANS_CANDIDATES) {
        fonts.font_data.insert("honya_sans".into(), Arc::new(data));
        prepend(&mut fonts, FontFamily::Proportional, "honya_sans");
    }
    if let Some(data) = load_first(CJK_CANDIDATES) {
        fonts.font_data.insert("honya_cjk".into(), Arc::new(data));
        prepend(&mut fonts, FontFamily::Proportional, "honya_cjk");
        prepend(&mut fonts, FontFamily::Monospace, "honya_cjk");
    }
    if let Some(data) = load_first(THAI_CANDIDATES) {
        fonts.font_data.insert("honya_thai".into(), Arc::new(data));
        prepend(&mut fonts, FontFamily::Proportional, "honya_thai");
        prepend(&mut fonts, FontFamily::Monospace, "honya_thai");
    }
    if let Some(data) = load_first(MONO_CANDIDATES) {
        fonts.font_data.insert("honya_mono".into(), Arc::new(data));
        prepend(&mut fonts, FontFamily::Monospace, "honya_mono");
    }

    ctx.set_fonts(fonts);
}

fn load_first(paths: &[&str]) -> Option<FontData> {
    for p in paths {
        if Path::new(p).is_file()
            && let Ok(bytes) = std::fs::read(p)
        {
            return Some(FontData::from_owned(bytes));
        }
    }
    None
}

fn prepend(fonts: &mut FontDefinitions, family: FontFamily, name: &str) {
    let entry = fonts.families.entry(family).or_default();
    entry.retain(|n| n != name);
    entry.insert(0, name.to_owned());
}
