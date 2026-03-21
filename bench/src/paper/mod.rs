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

    // ── metadata ops figure ──
    let meta = meta_ops_figure::render(results, &paper_dir)?;
    artifacts.push(meta);

    // ── paper-report.html ──
    render_index(&artifacts, out_dir)?;

    Ok(())
}

struct Artifact {
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
        h2 { font-size: 1.05em; color: #555; margin-top: 1.5em; }\n\
        ul { line-height: 1.6; }\n\
        iframe { border: 1px solid #ddd; background: #fff; }\n\
    </style>\n");
    html.push_str("</head><body>\n");
    html.push_str("<h1>Paper Artifacts</h1>\n");

    for art in artifacts {
        html.push_str(&format!("<h2>{}</h2>\n<ul>\n", crate::report::escape_html(&art.title)));
        html.push_str(&format!(
            "<li><a href=\"{}\">.tex source</a></li>\n",
            crate::report::escape_html(&art.tex_path)
        ));
        if let Some(ref pdf) = art.pdf_path {
            html.push_str(&format!(
                "<li><a href=\"{}\">.pdf</a></li>\n",
                crate::report::escape_html(pdf)
            ));
        }
        html.push_str("</ul>\n");
        if let Some(ref pdf) = art.pdf_path {
            // Embed with a reasonable default; the PDF is cropped to content.
            html.push_str(&format!(
                "<iframe src=\"{}\" style=\"width:100%;height:320px\"></iframe>\n",
                crate::report::escape_html(pdf)
            ));
        }
    }

    html.push_str("</body></html>\n");
    let path = out_dir.join("paper-report.html");
    std::fs::write(&path, html).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}
