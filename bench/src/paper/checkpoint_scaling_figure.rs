//! Publication figure: checkpoint depth scaling (create + read latency).

use super::Artifact;
use anyhow::{Context, Result};
use std::path::Path;

const CAPTION: &str =
    "File operation latency vs.\\ checkpoint depth (100 files per checkpoint). \
     TODO.";
const LABEL: &str = "fig:checkpoint-scaling";

pub fn artifact_meta(paper_dir: &Path) -> Artifact {
    let tex_path = paper_dir.join("checkpoint-scaling-figure.tex");
    let plot_pdf = paper_dir.join("checkpoint-scaling-figure-plot.pdf");
    Artifact {
        group: None,
        title: "Checkpoint scaling".to_string(),
        preferred: true,
        tex_path: format!("paper/{}", tex_path.file_name().unwrap().to_string_lossy()),
        pdf_path: None,
        tex_abs: tex_path,
        plot_pdfs: vec![plot_pdf],
    }
}

pub fn render(out_dir: &Path, paper_dir: &Path) -> Result<Artifact> {
    let preamble = super::util::plot_preamble();

    let json_path = out_dir.join("checkpoint-scaling.json");
    if !json_path.exists() {
        anyhow::bail!("checkpoint-scaling.json not found — run `agfs-bench checkpoint-scaling` first");
    }

    #[derive(serde::Deserialize)]
    struct CheckpointScalingResult {
        #[serde(default)]
        backend: String,
        mode: String,
        points: Vec<CheckpointScalingPoint>,
    }
    #[derive(serde::Deserialize)]
    struct CheckpointScalingPoint {
        depth: usize,
        mean_us: f64,
        #[allow(dead_code)]
        p50_us: f64,
    }

    let data: Vec<CheckpointScalingResult> =
        serde_json::from_str(&std::fs::read_to_string(&json_path)?)?;

    // Build CSV for the python script.
    let mut data_lines = Vec::new();
    for res in &data {
        let label = if res.backend.is_empty() {
            res.mode.clone()
        } else {
            format!("{} ({})", res.mode, res.backend)
        };
        for p in &res.points {
            data_lines.push(format!("{},{},{:.2}", label, p.depth, p.mean_us));
        }
    }

    let py_path = paper_dir.join("checkpoint-scaling-figure.py");
    let pdf_path = paper_dir.join("checkpoint-scaling-figure-plot.pdf");

    let script = format!(
        r#"{preamble}

DATA = """\
mode,depth,mean_us
{data}
"""

reader = csv.DictReader(StringIO(DATA.strip()))
rows = list(reader)

plt.rcParams.update({{'font.size': 7, 'axes.labelsize': 7, 'xtick.labelsize': 6.5,
                      'ytick.labelsize': 6.5, 'legend.fontsize': 5.5}})

fig, ax = plt.subplots(figsize=(1.67, 1.3))

# Collect unique series names.
series = sorted(set(r['mode'] for r in rows))
colors = list(S.TABLEAU10.values())

for i, name in enumerate(series):
    pts = [(int(r['depth']), float(r['mean_us'])) for r in rows if r['mode'] == name]
    pts.sort()
    xs = [p[0] for p in pts]
    ys = [p[1] for p in pts]
    ax.plot(xs, ys, marker='o', markersize=3, linewidth=1.2,
            color=colors[i % len(colors)], label=name)

ax.set_xlabel('checkpoint depth', fontweight='bold')
ax.set_ylabel('mean latency (\u00b5s/file)')
ax.set_ylim(bottom=0)
ax.legend(loc='upper right', handlelength=1.5)

fig.tight_layout(pad=0.3)
fig.savefig('{pdf_path}', bbox_inches='tight', dpi=300)
plt.close(fig)
"#,
        preamble = preamble,
        data = data_lines.join("\n"),
        pdf_path = pdf_path.display(),
    );

    super::util::ensure_plot_style(paper_dir)?;
    std::fs::write(&py_path, &script)
        .with_context(|| format!("writing {}", py_path.display()))?;

    let out = std::process::Command::new("python3")
        .arg(&py_path)
        .output()
        .with_context(|| format!("running {}", py_path.display()))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("matplotlib failed: {stderr}");
    }

    let plot_pdf_name = pdf_path.file_name().unwrap().to_string_lossy();
    let tex_path = paper_dir.join("checkpoint-scaling-figure.tex");
    let tex = format!(
        "\\PassOptionsToPackage{{activate=false}}{{microtype}}\n\
         \\documentclass[sigplan,screen]{{acmart}}\n\
         \\settopmatter{{printacmref=false,printfolios=false}}\n\
         \\renewcommand\\footnotetextcopyrightpermission[1]{{}}\n\
         \\usepackage{{graphicx}}\n\
         \\begin{{document}}\n\
         \\thispagestyle{{empty}}\n\
         % --- BEGIN figure fragment (includable via \\input) ---\n\
         \\begin{{figure}}[t]\n\
         \\centering\n\
         \\includegraphics{{{plot_pdf_name}}}\n\
         \\caption{{{CAPTION}}}\n\
         \\label{{{LABEL}}}\n\
         \\end{{figure}}\n\
         % --- END figure fragment ---\n\
         \\end{{document}}\n",
    );
    std::fs::write(&tex_path, &tex)
        .with_context(|| format!("writing {}", tex_path.display()))?;

    let preview_pdf = match super::run_pdflatex_cropped(&tex_path, paper_dir) {
        Ok(p) => Some(format!("paper/{}", p.file_name().unwrap().to_string_lossy())),
        Err(e) => {
            eprintln!("  warning: checkpoint-scaling-figure: {e:#}");
            Some(format!("paper/{plot_pdf_name}"))
        }
    };

    Ok(Artifact {
        group: None,
        title: "Checkpoint scaling".to_string(),
        preferred: true,
        tex_path: format!("paper/{}", tex_path.file_name().unwrap().to_string_lossy()),
        pdf_path: preview_pdf,
        tex_abs: tex_path,
        plot_pdfs: vec![pdf_path],
    })
}
