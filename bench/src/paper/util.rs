//! Shared helpers for paper artifact generation.

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;

/// Stylized backend names for publication.
pub fn backend_display_name(name: &str) -> &'static str {
    match name {
        "native" => "Native",
        "agfs-no-perm" => "AgFS",
        "agfs-realistic" => "AgFS-R",
        "agfs" => "AgFS",
        "overlayfs" => "OverlayFS",
        "branchfs" => "BranchFS",
        "try" => "Try",
        "btrfs" => "Btrfs",
        _ => "Unknown",
    }
}

/// Escape special LaTeX characters.
pub fn latex_escape(s: &str) -> String {
    // Preserve known LaTeX commands.
    if s.starts_with('\\') && !s.contains(' ') {
        return s.to_string();
    }
    s.replace('\\', "\\textbackslash{}")
        .replace('&', "\\&")
        .replace('%', "\\%")
        .replace('$', "\\$")
        .replace('#', "\\#")
        .replace('_', "\\_")
        .replace('{', "\\{")
        .replace('}', "\\}")
        .replace('~', "\\textasciitilde{}")
        .replace('^', "\\textasciicircum{}")
}

/// Compile a .tex file to a cropped PDF using pdflatex + pdfcrop.
///
/// The .tex file should be a standalone document. The resulting PDF is
/// cropped to the content bounding box so it embeds tightly in HTML.
///
/// Returns the path to the cropped PDF, or an error if compilation fails.
pub fn run_pdflatex_cropped(tex_path: &Path, output_dir: &Path) -> Result<std::path::PathBuf> {
    // Compile .tex → .pdf (two passes — acmart needs a second pass for page counting).
    let mut out = Command::new("pdflatex")
        .arg("-interaction=nonstopmode")
        .arg("-halt-on-error")
        .arg("-output-directory")
        .arg(output_dir)
        .arg(tex_path)
        .output()
        .with_context(|| format!("running pdflatex on {}", tex_path.display()))?;

    if out.status.success() {
        out = Command::new("pdflatex")
            .arg("-interaction=nonstopmode")
            .arg("-halt-on-error")
            .arg("-output-directory")
            .arg(output_dir)
            .arg(tex_path)
            .output()
            .with_context(|| format!("running pdflatex (pass 2) on {}", tex_path.display()))?;
    }

    let stem = tex_path.file_stem().unwrap().to_string_lossy();
    let pdf_path = output_dir.join(format!("{stem}.pdf"));

    if !out.status.success() || !pdf_path.exists() {
        let log_path = output_dir.join(format!("{stem}.log"));
        let log_excerpt = std::fs::read_to_string(&log_path)
            .ok()
            .map(|s| {
                s.lines()
                    .rev()
                    .take(30)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        bail!(
            "pdflatex failed for {}\nlog (tail):\n{}",
            tex_path.display(),
            log_excerpt
        );
    }

    // Crop to content bounding box.
    let cropped = output_dir.join(format!("{stem}-crop.pdf"));
    let crop_out = Command::new("pdfcrop")
        .arg(&pdf_path)
        .arg(&cropped)
        .output();

    match crop_out {
        Ok(o) if o.status.success() && cropped.exists() => {
            // Replace the original with the cropped version.
            std::fs::rename(&cropped, &pdf_path)
                .with_context(|| "replacing PDF with cropped version")?;
        }
        _ => {
            // pdfcrop not available — use uncropped PDF.
            eprintln!("  pdfcrop not available, using uncropped PDF");
        }
    }

    Ok(pdf_path)
}
