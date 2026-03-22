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
    tex_path: String,
    pdf_path: Option<String>,
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
                html.push_str(&format!(
                    "<div class=\"variant\">\n<h3>{}</h3>\n<ul>\n",
                    esc(&a.title)
                ));
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
                html.push_str("</div>\n");
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
