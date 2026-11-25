//! Bundled font registration, resolved relative to the executable.
//!
//! The renderer's family names (`Pretendard`, `檎风黑体 CHS`) only resolve if
//! the faces are installed in the text system before the first layout. A debug
//! run puts the exe in `target/debug`, a shipped run puts it in
//! `dist-experiment`; both carry `assets/fonts/*.ttf` beside them, so the same
//! lookup serves the dev tree and the installed bundle. A missing face is
//! reported and *not* substituted: the platform fallback chain takes over, and
//! the record says which family was asked for, so a CJK mismatch is visible in
//! the delivery notes instead of silently reshaping the window.
//!
//! The text system reports bundled and platform-installed families through one
//! list, so "the family resolves" and "we shipped the face" are different
//! facts. [`Registration`] keeps both: the delivery record must not call a face
//! bundled when it came from the machine's own font directory.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// One face found on disk.
pub struct Face {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
}

/// What the text system knows after registration, split by where it came from.
pub struct Registration {
    /// Every family name the text system reports.
    pub all: Vec<String>,
    /// Families that only appeared because this process registered bytes from
    /// `assets/fonts`, i.e. the ones the delivery bundle actually carries.
    pub bundled: Vec<String>,
}

/// Candidate directories, in order: beside the exe, then beside the manifest.
///
/// The second entry is what makes `cargo run` work: the exe lives in
/// `target/<profile>` while the assets live in the crate's `../../assets`.
pub fn search_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            out.push(dir.join("assets"));
            out.push(dir.join("assets").join("fonts"));
            // `target/debug/overlyrics.exe` -> `native/assets`.
            out.push(dir.join("..").join("..").join("assets"));
            out.push(dir.join("..").join("..").join("assets").join("fonts"));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        out.push(cwd.join("assets"));
        out.push(cwd.join("assets").join("fonts"));
        out.push(cwd.join("native").join("assets"));
        out.push(cwd.join("native").join("assets").join("fonts"));
    }
    let manifest_assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("assets");
    out.push(manifest_assets.clone());
    out.push(manifest_assets.join("fonts"));
    out
}

/// Read every `.ttf`/`.otf` the search finds. Duplicate file names keep the
/// first hit, so an installed bundle is never mixed with the dev tree.
pub fn collect() -> Vec<Face> {
    let mut faces: Vec<Face> = Vec::new();
    let mut seen = BTreeSet::new();
    for dir in search_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase);
            if !matches!(ext.as_deref(), Some("ttf") | Some("otf")) {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_string)
            else {
                continue;
            };
            if !seen.insert(name.clone()) {
                continue;
            }
            // Pretendard.ttf and PretendardVariable.ttf are both 6.7 MB variable
            // faces for the same family; loading only one saves ~6.7 MB RAM.
            let lower = name.to_ascii_lowercase();
            if lower == "pretendardvariable.ttf" && seen.contains("Pretendard.ttf")
                || lower == "pretendard.ttf" && seen.contains("PretendardVariable.ttf")
            {
                continue;
            }
            if let Ok(bytes) = std::fs::read(&path) {
                faces.push(Face { path, bytes });
            }
        }
    }
    faces
}

/// Register the faces once and report what the text system ended up with.
///
/// The bundled set is the difference between the family list before and after
/// `add_fonts`; a name that was already there came from the platform, and a
/// name that appears afterwards came from the bytes beside the executable.
pub fn register(
    text_system: &std::sync::Arc<gpui_kit::TextSystem>,
) -> anyhow::Result<Registration> {
    const EMBEDDED_PRETENDARD: &[u8] =
        include_bytes!("../../../../assets/fonts/Pretendard.ttf");

    let before: BTreeSet<String> = text_system.all_font_names().into_iter().collect();
    let faces = collect();
    let payload: Vec<std::borrow::Cow<'static, [u8]>> = if faces.is_empty() {
        log::info!("no on-disk fonts beside the executable; using embedded Pretendard face");
        vec![std::borrow::Cow::Borrowed(EMBEDDED_PRETENDARD)]
    } else {
        faces
            .into_iter()
            .map(|face| {
                log::info!("registering font {}", face.path.display());
                std::borrow::Cow::Owned(face.bytes)
            })
            .collect()
    };
    if !payload.is_empty() {
        text_system.add_fonts(payload)?;
    }
    let all = text_system.all_font_names();
    let bundled = all
        .iter()
        .filter(|name| !before.contains(*name))
        .cloned()
        .collect();
    Ok(Registration { all, bundled })
}

/// Which families the layout asks for, for the delivery record.
pub fn requested_families() -> (&'static str, &'static str) {
    ("Pretendard", "檎风黑体 CHS")
}

/// True when `names` contains a family the request needs (case-insensitive,
/// because the bundled face reports itself as `Pretendard`).
pub fn has_family(names: &[String], family: &str) -> bool {
    names
        .iter()
        .any(|name| name.eq_ignore_ascii_case(family) || name.contains(family))
}

/// Where one requested family came from, as the delivery record states it.
pub fn source_of(registration: &Registration, family: &str) -> &'static str {
    if has_family(&registration.bundled, family) {
        "bundled"
    } else if has_family(&registration.all, family) {
        "platform-installed"
    } else {
        "missing -> platform fallback"
    }
}

/// One-line CJK note. The bundled CJK face is not shipped (licence), so this
/// normally reports the platform face that actually rendered and never claims
/// the bundle carries it.
pub fn cjk_status(registration: &Registration) -> String {
    let (_, cjk) = requested_families();
    format!("{cjk}: {}", source_of(registration, cjk))
}

/// Path helper for the delivery bundle: the exe's own directory.
pub fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_pretendard_faces_are_discoverable_from_search_dirs() {
        let faces = collect();
        let has_pretendard = faces.iter().any(|f| {
            f.path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("Pretendard"))
        });
        assert!(has_pretendard, "search_dirs should locate bundled Pretendard font faces");
    }
}
