//! Palette inputs and an offline library. Render selections contain their colours, so the
//! active palette survives even if the library is moved or its recent list is pruned.

use std::fs;
use std::path::Path;
use std::process::Command;

use eframe::egui::Color32;
use serde::{Deserialize, Serialize};

use super::pixel::{self, Palette};

const MAX_COLORS: usize = 4096;
const MAX_INPUT: usize = MAX_COLORS * 10;
const RECENT_LIMIT: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    Lospec(String),
    Colors(Vec<String>),
}

pub fn hex_color(input: &str) -> Result<Color32, String> {
    let hex = input.strip_prefix('#').unwrap_or(input);
    if !matches!(hex.len(), 3 | 6) || !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!(
            "Invalid hex colour ‘{input}’; use #RGB or #RRGGBB."
        ));
    }
    let hex = if hex.len() == 3 {
        hex.chars().flat_map(|c| [c, c]).collect::<String>()
    } else {
        hex.to_owned()
    };
    let n = u32::from_str_radix(&hex, 16).map_err(|e| e.to_string())?;
    Ok(Color32::from_rgb((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

fn color_hex(c: Color32) -> String {
    format!("{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

fn colors(input: &str) -> Result<Vec<String>, String> {
    let parts: Vec<_> = input
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '-'))
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() || parts.len() > MAX_COLORS {
        return Err(format!("Use 1–{MAX_COLORS} hex colours."));
    }
    let mut result = Vec::new();
    for part in parts {
        let hex = color_hex(hex_color(part)?);
        if !result.contains(&hex) {
            result.push(hex);
        }
    }
    Ok(result)
}

impl Input {
    pub fn parse(input: &str) -> Result<Self, String> {
        let input = input.trim();
        if input.is_empty() || input.len() > MAX_INPUT {
            return Err("Paste a Lospec name, palette URL or hex colours.".into());
        }
        let url = if input.starts_with("lospec.com/") || input.starts_with("coolors.co/") {
            Some(format!("https://{input}"))
        } else if input.contains("://") {
            Some(input.to_owned())
        } else {
            None
        };
        if let Some(url) = url {
            let url = url::Url::parse(&url).map_err(|e| format!("Invalid palette URL: {e}"))?;
            if !matches!(url.scheme(), "https" | "http")
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err("Use a Lospec or Coolors http(s) palette URL.".into());
            }
            let path = url.path().trim_matches('/');
            return match url.host_str() {
                Some("lospec.com" | "www.lospec.com") => {
                    let slug = path
                        .strip_prefix("palette-list/")
                        .ok_or("Use lospec.com/palette-list/<slug>.")?;
                    Self::lospec(slug.strip_suffix(".json").unwrap_or(slug))
                }
                Some("coolors.co" | "www.coolors.co") => Ok(Self::Colors(colors(
                    path.strip_prefix("palette/").unwrap_or(path),
                )?)),
                _ => Err("Unsupported website; use Lospec, Coolors or paste hex colours.".into()),
            };
        }
        if let Ok(colors) = colors(input) {
            return Ok(Self::Colors(colors));
        }
        if input.contains(['#', ',', ';'])
            || input
                .split('-')
                .next()
                .is_some_and(|s| s.len() == 6 && s.bytes().all(|c| c.is_ascii_hexdigit()))
        {
            return Err(
                "Invalid hex list; separate #RGB or #RRGGBB colours with spaces, commas or dashes."
                    .into(),
            );
        }
        Self::lospec(
            &input
                .split_whitespace()
                .collect::<Vec<_>>()
                .join("-")
                .to_ascii_lowercase(),
        )
    }

    fn lospec(slug: &str) -> Result<Self, String> {
        if slug.is_empty()
            || slug.len() > 160
            || slug.starts_with('-')
            || slug.ends_with('-')
            || !slug
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        {
            return Err("Invalid Lospec name; use a slug such as ice-cream-gb.".into());
        }
        Ok(Self::Lospec(slug.into()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Imported {
    pub name: String,
    pub source: String,
    pub author: Option<String>,
    pub colors: Vec<String>,
}

impl Imported {
    pub fn from_json(slug: &str, json: &str) -> Result<Self, String> {
        let palette =
            Palette::from_lospec(json).map_err(|e| format!("Invalid Lospec palette: {e}"))?;
        if palette.colors.len() > MAX_COLORS {
            return Err(format!("Palette exceeds {MAX_COLORS} colours."));
        }
        let metadata: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
        Ok(Self {
            name: palette.name.chars().take(100).collect(),
            source: format!("https://lospec.com/palette-list/{slug}"),
            author: metadata["author"]
                .as_str()
                .map(|s| s.chars().take(100).collect()),
            colors: colors(
                &palette
                    .colors
                    .into_iter()
                    .map(color_hex)
                    .collect::<Vec<_>>()
                    .join("-"),
            )?,
        })
    }

    pub fn selection(&self) -> String {
        format!("hex:{}", self.colors.join("-"))
    }

    pub fn palette(&self) -> Palette {
        Palette {
            name: self.name.clone(),
            colors: self
                .colors
                .iter()
                .filter_map(|c| hex_color(c).ok())
                .collect(),
        }
    }
}

/// Fetch only canonical Lospec endpoints; pasted URLs never become arbitrary requests.
/// curl is also used for model downloads and supplies the platform's HTTPS trust store.
pub fn fetch(slug: &str) -> Result<Imported, String> {
    Input::lospec(slug)?;
    let output = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--connect-timeout",
            "5",
            "--max-time",
            "15",
            "--max-filesize",
            "131072",
            "--proto",
            "=https",
        ])
        .arg(format!("https://lospec.com/palette-list/{slug}.json"))
        .output()
        .map_err(|e| format!("Could not fetch Lospec palette: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "Lospec ‘{slug}’: {} Check the name and connection, then retry. Cached imports still work offline.",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let json = std::str::from_utf8(&output.stdout).map_err(|_| "Lospec returned invalid text.")?;
    Imported::from_json(slug, json)
}

/// Resolves a persisted selection without disk access or networking in the renderer.
pub fn resolve(selection: &str) -> Option<Palette> {
    if let Some(hex) = selection.strip_prefix("hex:") {
        let colors = colors(hex)
            .ok()?
            .iter()
            .map(|c| hex_color(c))
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        Some(Palette {
            name: "Imported palette".into(),
            colors,
        })
    } else {
        pixel::BUNDLED
            .iter()
            .find(|(name, _)| *name == selection)
            .and_then(|(_, json)| Palette::from_lospec(json).ok())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Library {
    pub imports: Vec<Imported>,
    pub recent: Vec<String>,
}

impl Library {
    pub fn load(path: &Path) -> Result<Self, String> {
        let json = match fs::read_to_string(path) {
            Ok(json) => json,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("Read palette cache: {e}")),
        };
        let mut library: Self =
            serde_json::from_str(&json).map_err(|e| format!("Read palette cache: {e}"))?;
        if library.imports.iter().any(|p| {
            p.colors.is_empty()
                || p.colors.len() > MAX_COLORS
                || p.colors.iter().any(|c| hex_color(c).is_err())
        }) {
            return Err("Palette cache contains invalid colours.".into());
        }
        library.recent.truncate(RECENT_LIMIT);
        Ok(library)
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let save = || -> std::io::Result<()> {
            let dir = path.parent().unwrap_or(Path::new("."));
            fs::create_dir_all(dir)?;
            let temp = path.with_extension(format!("{}.tmp", std::process::id()));
            fs::write(&temp, serde_json::to_vec_pretty(self)?)?;
            fs::rename(temp, path)
        };
        save().map_err(|e| format!("Could not save palette cache: {e}"))
    }

    pub fn remember(&mut self, selection: &str) {
        self.recent.retain(|id| id != selection);
        self.recent.insert(0, selection.into());
        self.recent.truncate(RECENT_LIMIT);
    }

    pub fn insert(&mut self, palette: Imported) {
        self.remember(&palette.selection());
        self.imports.retain(|p| p.source != palette.source);
        self.imports.push(palette);
    }

    /// Cache lookup precedes a fetch, including when a previously imported slug is pasted.
    pub fn cached(&self, input: &Input) -> Option<Imported> {
        match input {
            Input::Colors(colors) => Some(Imported {
                name: format!("Hex · {} colours · #{}", colors.len(), colors[0]),
                source: format!("hex:{}", colors.join("-")),
                author: None,
                colors: colors.clone(),
            }),
            Input::Lospec(slug) => self
                .imports
                .iter()
                .find(|p| p.source == format!("https://lospec.com/palette-list/{slug}"))
                .cloned(),
        }
    }

    pub fn name(&self, selection: &str) -> String {
        if selection.is_empty() {
            return "Own colours".into();
        }
        self.imports
            .iter()
            .find(|p| p.selection() == selection)
            .map(|p| p.name.clone())
            .or_else(|| resolve(selection).map(|p| p.name))
            .unwrap_or_else(|| "Unavailable palette".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const REAL: [(&str, &str, usize); 3] = [
        (
            "ice-cream-gb",
            include_str!("../../../docs/assets/tests/custom-palettes/ice-cream-gb.json"),
            4,
        ),
        (
            "endesga-16",
            include_str!("../../../docs/assets/tests/custom-palettes/endesga-16.json"),
            16,
        ),
        (
            "resurrect-64",
            include_str!("../../../docs/assets/tests/custom-palettes/resurrect-64.json"),
            64,
        ),
    ];

    #[test]
    fn parses_names_urls_and_hex_codes() {
        for input in [
            "ice-cream-gb",
            "Ice Cream GB",
            "https://lospec.com/palette-list/ice-cream-gb",
            "https://lospec.com/palette-list/ice-cream-gb.json?download=1",
            "lospec.com/palette-list/ice-cream-gb/",
        ] {
            assert_eq!(
                Input::parse(input).unwrap(),
                Input::Lospec("ice-cream-gb".into())
            );
        }
        let expected = Input::Colors(vec!["264653".into(), "2a9d8f".into(), "e9c46a".into()]);
        for input in [
            "264653-2a9d8f-e9c46a",
            "https://coolors.co/264653-2a9d8f-e9c46a",
            "https://coolors.co/palette/264653-2a9d8f-e9c46a",
            "#264653, #2A9D8F; #e9c46a",
            "264653\n2a9d8f e9c46a",
        ] {
            assert_eq!(Input::parse(input).unwrap(), expected, "{input}");
        }
        assert_eq!(
            Input::parse("#abc #ABC #fff").unwrap(),
            Input::Colors(vec!["aabbcc".into(), "ffffff".into()])
        );
    }

    #[test]
    fn malformed_inputs_never_become_fetch_targets() {
        for input in [
            "",
            "#12",
            "#12345",
            "#12345678",
            "#ggg",
            "264653-zzzzzz",
            "https://example.com/palette",
            "file:///tmp/palette",
            "https://lospec.com/other/name",
            "https://lospec.com/palette-list/a/b",
            "https://lospec.com@localhost/palette-list/foo",
            "../../foo",
            "https://coolors.co/not-a-palette",
        ] {
            assert!(Input::parse(input).is_err(), "{input}");
        }
        for json in [
            "not json",
            r#"{"colors":[]}"#,
            r#"{"colors":[null]}"#,
            r#"{"colors":["12345678"]}"#,
        ] {
            assert!(Imported::from_json("test", json).is_err());
        }
    }

    #[test]
    fn cache_keeps_every_import_when_recents_roll_over_and_reopens_offline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("palettes.json");
        let mut library = Library::load(&path).unwrap();
        for (slug, json, count) in REAL {
            let palette = Imported::from_json(slug, json).unwrap();
            assert_eq!(palette.colors.len(), count);
            library.insert(palette);
        }
        for i in 0..12 {
            library.remember(&format!("preset-{i}"));
        }
        library.save(&path).unwrap();
        let library = Library::load(&path).unwrap();
        assert_eq!(library.recent.len(), RECENT_LIMIT);
        for (slug, _, count) in REAL {
            let cached = library.cached(&Input::parse(slug).unwrap()).unwrap();
            assert_eq!(resolve(&cached.selection()).unwrap().colors.len(), count);
        }
        // Active settings carry the palette independently of the library file.
        let selection = library.imports[0].selection();
        fs::remove_file(path).unwrap();
        assert_eq!(resolve(&selection).unwrap().colors.len(), 4);
    }

    #[test]
    fn corrupted_cache_reports_error_without_replacing_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("palettes.json");
        fs::write(&path, "broken").unwrap();
        assert!(
            Library::load(&path)
                .unwrap_err()
                .contains("Read palette cache")
        );
        assert_eq!(fs::read_to_string(path).unwrap(), "broken");
    }

    #[test]
    fn imported_roles_have_contrast_and_preserve_alpha_across_real_and_extreme_palettes() {
        use super::super::pixel::{Mapper, contrast};
        let mut palettes: Vec<_> = REAL
            .into_iter()
            .map(|(slug, json, _)| Imported::from_json(slug, json).unwrap().palette())
            .collect();
        for hex in [
            "#fff",
            "#000",
            "#aaa #bbb #ccc #ddd",
            "#010101 #020202 #030303 #040404",
            "#f00 #0f0 #00f #ff0",
        ] {
            palettes.push(
                Library::default()
                    .cached(&Input::parse(hex).unwrap())
                    .unwrap()
                    .palette(),
            );
        }
        for palette in palettes {
            let mut mapper = Mapper::imported(&palette);
            let (ink, text) = mapper.ink_and_text().unwrap();
            assert!(
                contrast(ink, text) >= 7.0,
                "{}: {}",
                palette.name,
                contrast(ink, text)
            );
            assert_eq!(mapper.map(Color32::from_rgb(30, 22, 20)), ink);
            assert_eq!(mapper.map(Color32::from_rgb(243, 233, 210)), text);
            let faded = mapper.translucent(Color32::from_rgba_unmultiplied(243, 233, 210, 100));
            assert_eq!(faded.a(), 100);
            assert_eq!(
                mapper.translucent(Color32::TRANSPARENT),
                Color32::TRANSPARENT
            );
            let tones: std::collections::HashSet<_> = (0..=255)
                .map(|v| mapper.map(Color32::from_gray(v)))
                .collect();
            assert!(tones.len() >= 3, "{}: {} tones", palette.name, tones.len());
        }
    }
    #[test]
    fn coloured_signs_get_readable_text_and_a_plate_in_imported_palettes() {
        use eframe::egui::{self, FontId, Pos2, Shape};
        let palette = Imported::from_json(REAL[0].0, REAL[0].1).unwrap().palette();
        let mut mapper = pixel::Mapper::imported(&palette);
        let (ink, foreground) = mapper.ink_and_text().unwrap();
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            let galley = ui.painter().layout_no_wrap(
                "LEDGER · 12".into(),
                FontId::proportional(14.0),
                Color32::GOLD,
            );
            let mut shape = Shape::Text(egui::epaint::TextShape::new(
                Pos2::ZERO,
                galley,
                Color32::GOLD,
            ));
            mapper.shape(&mut shape);
            let Shape::Vec(shapes) = shape else {
                panic!("expected a plate and text")
            };
            let Shape::Rect(plate) = &shapes[0] else {
                panic!("expected plate")
            };
            let Shape::Text(text) = &shapes[1] else {
                panic!("expected text")
            };
            assert_eq!(plate.fill, ink);
            assert_eq!(text.override_text_color, Some(foreground));
            assert!(pixel::contrast(plate.fill, text.override_text_color.unwrap()) >= 7.0);
        });
        output.textures_delta.clear();
    }
}
