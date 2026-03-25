//! Publication figure: checkpoint depth scaling (split create/read latency).

use super::Artifact;
use anyhow::{Context, Result};
use std::path::Path;

const CAPTION: &str = "Checkpoint scalability. The latency of creating a new file or reading an existing file as the number of checkpoints grow. OverlayFS fails to support more checkpoints because of mount option limits.";
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
        anyhow::bail!(
            "checkpoint-scaling.json not found — run `agfs-bench checkpoint-scaling` first"
        );
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
    }

    let data: Vec<CheckpointScalingResult> =
        serde_json::from_str(&std::fs::read_to_string(&json_path)?)?;

    let mut create_lines = Vec::new();
    let mut read_lines = Vec::new();
    for res in &data {
        if res.backend == "agfs-no-perm" {
            continue;
        }
        // In this figure agfs-realistic is the only AgFS variant, so label it
        // simply "AgFS" rather than "AgFS-R".
        let label = match res.backend.as_str() {
            "agfs-realistic" => "AgFS",
            other => super::util::backend_display_name(other),
        }
        .to_string();
        for p in &res.points {
            let row = format!("{},{},{:.2}", label, p.depth, p.mean_us);
            if res.mode == "create" {
                create_lines.push(row);
            } else if res.mode == "read" {
                read_lines.push(row);
            }
        }
    }

    let py_path = paper_dir.join("checkpoint-scaling-figure.py");
    let pdf_path = paper_dir.join("checkpoint-scaling-figure-plot.pdf");

    let script = format!(
        r#"{preamble}

CREATE_DATA = """\
backend,depth,mean_us
{create_data}
"""

READ_DATA = """\
backend,depth,mean_us
{read_data}
"""

plt.rcParams.update({{'font.size': 7, 'axes.labelsize': 7, 'xtick.labelsize': 6.5,
                      'ytick.labelsize': 6.5, 'legend.fontsize': 6}})

order = ['AgFS', 'OverlayFS', 'BranchFS']
colors = [S.BACKEND_COLORS.get(n, S.TABLEAU10['gray']) for n in order]

fig, (ax_create, ax_read) = plt.subplots(1, 2, sharey=False, figsize=(3.33, 1.3),
                                          gridspec_kw={{'wspace': 0.15}})

def plot_panel(ax, data_csv, xlabel, show_ylabel):
    reader = csv.DictReader(StringIO(data_csv.strip()))
    rows = list(reader)
    for i, name in enumerate(order):
        pts = [(int(r['depth']), float(r['mean_us'])) for r in rows if r['backend'] == name]
        if not pts:
            continue
        pts.sort()
        xs = [p[0] for p in pts]
        ys = [p[1] for p in pts]
        ax.plot(xs, ys, marker='o', markersize=2.5, linewidth=1.2,
                color=colors[i], label=name)
    ax.set_xlabel(xlabel, fontweight='bold', fontsize=6.5)
    ax.set_ylim(bottom=0)
    if show_ylabel:
        ax.set_ylabel('latency (\u00b5s/op)')
    else:
        ax.tick_params(axis='y', labelleft=False)

plot_panel(ax_create, CREATE_DATA, 'create', show_ylabel=True)
plot_panel(ax_read, READ_DATA, 'read', show_ylabel=False)

# Shared legend at top.
handles, labels = ax_create.get_legend_handles_labels()
fig.legend(handles=handles, labels=labels, loc='upper center',
           bbox_to_anchor=(0.5, 1.0), ncol=len(order),
           handlelength=1.5, handletextpad=0.4,
           borderpad=0.2, columnspacing=0.8)

fig.tight_layout(pad=0.3)

# Shared x-axis label below both panels.
fig.canvas.draw()
xlabel_bb = ax_create.xaxis.label.get_window_extent(fig.canvas.get_renderer())
xlabel_bottom_fig = fig.transFigure.inverted().transform((0, xlabel_bb.y0))[1]
fig.text(0.5, xlabel_bottom_fig - 0.06, 'number of checkpoints', ha='center', fontsize=7)

fig.savefig('{pdf_path}', bbox_inches='tight', dpi=300)
plt.close(fig)
"#,
        preamble = preamble,
        create_data = create_lines.join("\n"),
        read_data = read_lines.join("\n"),
        pdf_path = pdf_path.display(),
    );

    super::util::ensure_plot_style(paper_dir)?;
    std::fs::write(&py_path, &script).with_context(|| format!("writing {}", py_path.display()))?;

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
    std::fs::write(&tex_path, &tex).with_context(|| format!("writing {}", tex_path.display()))?;

    let preview_pdf = match super::run_pdflatex_cropped(&tex_path, paper_dir) {
        Ok(p) => Some(format!(
            "paper/{}",
            p.file_name().unwrap().to_string_lossy()
        )),
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
