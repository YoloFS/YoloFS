//! Publication-ready artifacts: LaTeX tables, figures, and HTML index.

pub mod fio_data_table;
pub mod meta_ops_figure;
mod util;

pub use util::{backend_display_name, latex_escape, run_pdflatex_cropped};

use anyhow::{Context, Result};
use std::path::Path;

/// Render all paper artifacts and the paper-report.html index.
pub fn render(results: &crate::BenchResults, out_dir: &Path) -> Result<()> {
    let paper_dir = out_dir.join("paper");
    std::fs::create_dir_all(&paper_dir)
        .with_context(|| format!("creating {}", paper_dir.display()))?;

    let mut artifacts: Vec<Artifact> = Vec::new();

    // ── fio data table ──
    let fio = fio_data_table::render(results, &paper_dir)?;
    artifacts.push(fio);

    // ── metadata ops figure (multiple variants) ──
    artifacts.extend(meta_ops_figure::render(results, &paper_dir)?);

    // ── paper-report.html ──
    render_index(&artifacts, out_dir)?;

    Ok(())
}

struct Artifact {
    /// Group name for variants of the same figure (e.g. "Metadata operation latency").
    /// Artifacts with the same group are shown under a shared heading.
    /// `None` means standalone (gets its own heading from `title`).
    group: Option<String>,
    /// Variant-specific title (e.g. "broken axis", "capped + annotated").
    title: String,
    /// Preferred variant is shown expanded; others collapsed.
    preferred: bool,
    /// Relative path for HTML links (e.g. "paper/foo.tex").
    tex_path: String,
    pdf_path: Option<String>,
    /// Absolute path to the generated .tex file (for install-paper extraction).
    tex_abs: std::path::PathBuf,
    /// Absolute paths to plot PDFs that must be copied alongside the .tex fragment.
    plot_pdfs: Vec<std::path::PathBuf>,
}

/// Install preferred paper artifacts into the paper repository.
///
/// Extracts float fragments (between BEGIN/END markers) from the generated
/// .tex files, copies associated plot PDFs, and writes everything to
/// `<paper_dir>/generated/`.
pub fn install(results: &crate::BenchResults, out_dir: &Path, paper_dir: &Path) -> Result<()> {
    if !paper_dir.join("main.tex").exists() {
        anyhow::bail!(
            "{} does not look like a paper repo (no main.tex)",
            paper_dir.display()
        );
    }

    // First, render all artifacts so we have the .tex and plot PDFs.
    let bench_paper_dir = out_dir.join("paper");
    std::fs::create_dir_all(&bench_paper_dir)?;

    let mut artifacts: Vec<Artifact> = Vec::new();
    artifacts.push(fio_data_table::render(results, &bench_paper_dir)?);
    artifacts.extend(meta_ops_figure::render(results, &bench_paper_dir)?);

    let gen_dir = paper_dir.join("generated");
    std::fs::create_dir_all(&gen_dir)
        .with_context(|| format!("creating {}", gen_dir.display()))?;

    let mut installed = Vec::new();

    for art in &artifacts {
        if !art.preferred {
            continue;
        }

        // Read the full .tex file and extract the float fragment.
        let tex_content = std::fs::read_to_string(&art.tex_abs)
            .with_context(|| format!("reading {}", art.tex_abs.display()))?;

        let fragment = extract_fragment(&tex_content).unwrap_or(&tex_content);

        // For figures, rewrite \includegraphics paths to reference generated/.
        let fragment = rewrite_includegraphics(fragment, "generated");

        let dest_tex = gen_dir.join(art.tex_abs.file_name().unwrap());
        std::fs::write(&dest_tex, &fragment)
            .with_context(|| format!("writing {}", dest_tex.display()))?;
        installed.push(format!("  tex: {}", dest_tex.display()));

        // Copy associated plot PDFs.
        for pdf in &art.plot_pdfs {
            if pdf.exists() {
                let dest_pdf = gen_dir.join(pdf.file_name().unwrap());
                std::fs::copy(pdf, &dest_pdf)
                    .with_context(|| format!("copying {} → {}", pdf.display(), dest_pdf.display()))?;
                installed.push(format!("  pdf: {}", dest_pdf.display()));
            }
        }
    }

    if installed.is_empty() {
        eprintln!("No preferred artifacts to install.");
    } else {
        eprintln!("Installed to {}:", gen_dir.display());
        for line in &installed {
            eprintln!("{line}");
        }
        eprintln!("\nInclude in your paper with:");
        for art in &artifacts {
            if !art.preferred {
                continue;
            }
            let name = art.tex_abs.file_name().unwrap().to_string_lossy();
            eprintln!("  \\input{{generated/{name}}}");
        }
    }

    Ok(())
}

/// Extract content between BEGIN/END fragment markers.
fn extract_fragment(tex: &str) -> Option<&str> {
    let start_marker = "% --- BEGIN ";
    let end_marker = "% --- END ";
    let start = tex.find(start_marker)?;
    let end = tex.find(end_marker)?;
    let end_line = tex[end..].find('\n').map(|i| end + i + 1).unwrap_or(tex.len());
    Some(&tex[start..end_line])
}

/// Rewrite `\includegraphics{foo.pdf}` to `\includegraphics{<prefix>/foo.pdf}`.
fn rewrite_includegraphics(fragment: &str, prefix: &str) -> String {
    fragment
        .lines()
        .map(|line| {
            if let Some(idx) = line.find("\\includegraphics") {
                if let Some(brace_start) = line[idx..].find('{') {
                    let abs_start = idx + brace_start + 1;
                    if let Some(brace_end) = line[abs_start..].find('}') {
                        let filename = &line[abs_start..abs_start + brace_end];
                        // Only rewrite if it's a bare filename (no path separators).
                        if !filename.contains('/') {
                            return format!(
                                "{}{{{prefix}/{filename}}}{}",
                                &line[..abs_start - 1],
                                &line[abs_start + brace_end + 1..]
                            );
                        }
                    }
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_index(artifacts: &[Artifact], out_dir: &Path) -> Result<()> {
    let mut html = String::new();
    html.push_str("<!DOCTYPE html><html><head><meta charset=\"utf-8\">\n");
    html.push_str("<title>agfs-bench paper report</title>\n");
    html.push_str("<style>\n\
        body { font-family: system-ui, sans-serif; margin: 1.5em; background: #fafafa; }\n\
        h1 { font-size: 1.2em; }\n\
        h2 { font-size: 1.05em; color: #333; margin-top: 1.5em; }\n\
        h3 { font-size: 0.95em; color: #666; margin-top: 1em; margin-left: 1em; }\n\
        .variant { margin-left: 1em; }\n\
        ul { line-height: 1.6; }\n\
        iframe { border: 1px solid #ddd; background: #fff; }\n\
    </style>\n");
    html.push_str("</head><body>\n");
    html.push_str("<h1>Paper Artifacts</h1>\n");

    let esc = crate::report::escape_html;

    let mut i = 0;
    while i < artifacts.len() {
        let art = &artifacts[i];

        if let Some(ref group) = art.group {
            // Emit group heading, then all consecutive artifacts with the same group.
            html.push_str(&format!("<h2>{}</h2>\n", esc(group)));
            let group_name = group.clone();
            while i < artifacts.len()
                && artifacts[i].group.as_deref() == Some(&group_name)
            {
                let a = &artifacts[i];
                if a.preferred {
                    // Preferred variant: shown expanded.
                    html.push_str(&format!(
                        "<div class=\"variant\">\n<h3>{}</h3>\n",
                        esc(&a.title)
                    ));
                } else {
                    // Non-preferred: collapsed.
                    html.push_str(&format!(
                        "<details class=\"variant\">\n<summary>{}</summary>\n",
                        esc(&a.title)
                    ));
                }
                html.push_str("<ul>\n");
                html.push_str(&format!(
                    "<li><a href=\"{}\">.tex source</a></li>\n",
                    esc(&a.tex_path)
                ));
                if let Some(ref pdf) = a.pdf_path {
                    html.push_str(&format!(
                        "<li><a href=\"{}\">.pdf</a></li>\n",
                        esc(pdf)
                    ));
                }
                html.push_str("</ul>\n");
                if let Some(ref pdf) = a.pdf_path {
                    html.push_str(&format!(
                        "<iframe src=\"{}\" style=\"width:100%;height:320px\"></iframe>\n",
                        esc(pdf)
                    ));
                }
                if a.preferred {
                    html.push_str("</div>\n");
                } else {
                    html.push_str("</details>\n");
                }
                i += 1;
            }
        } else {
            // Standalone artifact.
            html.push_str(&format!("<h2>{}</h2>\n<ul>\n", esc(&art.title)));
            html.push_str(&format!(
                "<li><a href=\"{}\">.tex source</a></li>\n",
                esc(&art.tex_path)
            ));
            if let Some(ref pdf) = art.pdf_path {
                html.push_str(&format!(
                    "<li><a href=\"{}\">.pdf</a></li>\n",
                    esc(pdf)
                ));
            }
            html.push_str("</ul>\n");
            if let Some(ref pdf) = art.pdf_path {
                html.push_str(&format!(
                    "<iframe src=\"{}\" style=\"width:100%;height:320px\"></iframe>\n",
                    esc(pdf)
                ));
            }
            i += 1;
        }
    }

    html.push_str("</body></html>\n");
    let path = out_dir.join("paper-report.html");
    std::fs::write(&path, html).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}
