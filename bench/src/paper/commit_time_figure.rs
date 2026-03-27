//! Publication figure: commit + status time per operation (session microbenchmarks).

use super::Artifact;
use crate::report;
use crate::BenchResults;
use anyhow::{Context, Result};
use std::path::Path;

const CAPTION: &str =
    "Commit and status cost per file operation (\\textmu s/op) for 10\\,000 files. \
     TODO.";
const LABEL: &str = "fig:commit-time";

const WORKLOADS: &[(&str, &str)] = &[
    ("write-files", "create"),
    ("overwrite-files", "overwrite"),
    ("rename-files", "rename"),
    ("unlink-files", "unlink"),
];

/// Backends to show (no native — no commit; no agfs-no-perm).
const BACKENDS: &[(&str, &str)] = &[
    ("agfs-realistic", "AgFS"),
    ("overlayfs", "OverlayFS"),
    ("branchfs", "BranchFS"),
];

const FILE_COUNT: f64 = 10_000.0;

pub fn artifact_meta(paper_dir: &Path) -> Artifact {
    let tex_path = paper_dir.join("commit-time-figure.tex");
    let plot_pdf = paper_dir.join("commit-time-figure-plot.pdf");
    Artifact {
        group: None,
        title: "Commit + status time per operation".to_string(),
        preferred: true,
        tex_path: format!("paper/{}", tex_path.file_name().unwrap().to_string_lossy()),
        pdf_path: None,
        tex_abs: tex_path,
        plot_pdfs: vec![plot_pdf],
    }
}

pub fn render(results: &BenchResults, paper_dir: &Path) -> Result<Artifact> {
    let preamble = super::util::plot_preamble();

    // Collect data: op,backend,metric,us_per_op
    let mut data_lines = Vec::new();
    for &(wl_name, op_label) in WORKLOADS {
        for &(backend_key, backend_label) in BACKENDS {
            let wl = results
                .workloads
                .iter()
                .find(|w| report::normalize_legacy_workload_name(&w.workload) == wl_name)
                .and_then(|wl| wl.backends.iter().find(|b| b.backend == backend_key));

            if let Some(b) = wl {
                let runs = &b.iterations;
                if runs.is_empty() {
                    continue;
                }
                let n = runs.len() as f64;

                let avg_commit: f64 = runs
                    .iter()
                    .map(|r| r.commit_ms.unwrap_or(0) as f64)
                    .sum::<f64>()
                    / n;
                data_lines.push(format!(
                    "{op_label},{backend_label},commit,{:.2}",
                    avg_commit / FILE_COUNT * 1000.0
                ));

                let avg_status_us: f64 = runs
                    .iter()
                    .map(|r| r.status_us.unwrap_or(0) as f64)
                    .sum::<f64>()
                    / n;
                if avg_status_us > 0.0 {
                    data_lines.push(format!(
                        "{op_label},{backend_label},status,{:.2}",
                        avg_status_us / FILE_COUNT
                    ));
                }
            }
        }
    }

    let py_path = paper_dir.join("commit-time-figure.py");
    let pdf_path = paper_dir.join("commit-time-figure-plot.pdf");

    let backend_labels: Vec<&str> = BACKENDS.iter().map(|(_, l)| *l).collect();
    let backends_py: String = backend_labels
        .iter()
        .map(|l| format!("'{l}'"))
        .collect::<Vec<_>>()
        .join(", ");

    let script = format!(
        r#"{preamble}
from io import StringIO
import csv

DATA = """\
op,backend,metric,us_per_op
{data}
"""

reader = csv.DictReader(StringIO(DATA.strip()))
rows = list(reader)

ops = {ops_py}
backends = [{backends_py}]
metrics = ['commit', 'status']

# Build lookup: (op, backend, metric) -> us_per_op
lookup = {{}}
for r in rows:
    lookup[(r['op'], r['backend'], r['metric'])] = float(r['us_per_op'])

backend_colors = {{
    'AgFS':       S.TABLEAU10['blue'],
    'OverlayFS':  S.TABLEAU10['green'],
    'BranchFS':   S.TABLEAU10['orange'],
}}

plt.rcParams.update({{'font.size': 7, 'axes.labelsize': 7, 'xtick.labelsize': 6.5,
                      'ytick.labelsize': 6.5, 'legend.fontsize': 6}})

nb = len(backends)
bar_height = 0.5 / nb
y = np.arange(len(ops)) * 0.7  # tighter spacing between groups

# Compute data ranges to set panel widths proportional (same µs/inch).
commit_max = max(lookup.get((op, b, 'commit'), 0) for op in ops for b in backends) * 1.1
status_max = max((lookup.get((op, b, 'status'), 0) for op in ops for b in backends), default=1) * 1.1
if status_max < 1:
    status_max = commit_max * 0.3  # fallback if no status data
ratio_commit = commit_max / (commit_max + status_max)
ratio_status = status_max / (commit_max + status_max)

fig, (ax_commit, ax_status) = plt.subplots(1, 2, sharey=True, figsize=(3.33, 0.9),
                                            gridspec_kw={{'width_ratios': [ratio_commit, ratio_status], 'wspace': 0.15}})

# Left panel: commit time.
for bi, b in enumerate(backends):
    vals = [lookup.get((op, b, 'commit'), 0) for op in ops]
    offset = ((nb - 1) / 2 - bi) * bar_height
    ax_commit.barh(y + offset, vals, bar_height * 0.85,
                   color=backend_colors.get(b, '#999'),
                   edgecolor='white', linewidth=0.3,
                   label=b)
ax_commit.set_xlabel('commit', fontweight='bold')
ax_commit.set_xlim(left=0)
ax_commit.set_yticks(y)
ax_commit.set_yticklabels([op for op in ops], fontweight='bold')
ax_commit.tick_params(axis='y', length=0, pad=4)

# Right panel: status time.
for bi, b in enumerate(backends):
    vals = [lookup.get((op, b, 'status'), 0) for op in ops]
    offset = ((nb - 1) / 2 - bi) * bar_height
    ax_status.barh(y + offset, vals, bar_height * 0.85,
                   color=backend_colors.get(b, '#999'),
                   edgecolor='white', linewidth=0.3)
    # Show N/A for backends with no status data.
    for oi, op in enumerate(ops):
        if (op, b, 'status') not in lookup:
            ax_status.text(0.5, y[oi] + offset, 'N/A', va='center',
                           fontsize=5, color='#999')
ax_status.set_xlabel('report', fontweight='bold')
ax_status.set_xlim(left=0)
ax_status.tick_params(axis='y', length=0)

# Set per-panel ranges (same physical scale via width_ratios).
from matplotlib.ticker import MaxNLocator
ax_commit.set_xlim(0, commit_max)
ax_status.set_xlim(0, status_max)
ax_commit.xaxis.set_major_locator(MaxNLocator(nbins=6, integer=True))
ax_status.xaxis.set_major_locator(MaxNLocator(nbins=4, integer=True))

# Legend at top, shared.
from matplotlib.patches import Patch
legend_items = [Patch(facecolor=backend_colors[b], edgecolor='white', label=b) for b in backends]
fig.legend(handles=legend_items, loc='upper center', bbox_to_anchor=(0.5, 1.0),
           ncol=nb, handlelength=1, handletextpad=0.4,
           borderpad=0.2, columnspacing=0.8)

fig.tight_layout(pad=0.3)

# Place "µs/file" centered below xlabels by measuring their position.
fig.canvas.draw()
# Get the bottom of the commit xlabel in figure coords.
xlabel_bb = ax_commit.xaxis.label.get_window_extent(fig.canvas.get_renderer())
xlabel_bottom_fig = fig.transFigure.inverted().transform((0, xlabel_bb.y0))[1]
fig.text(0.5, xlabel_bottom_fig - 0.06, '\u00b5s/file', ha='center', fontsize=7)
fig.savefig('{pdf_path}', bbox_inches='tight', dpi=300)
plt.close(fig)
"#,
        preamble = preamble,
        data = data_lines.join("\n"),
        ops_py = format!(
            "[{}]",
            WORKLOADS
                .iter()
                .map(|(_, l)| format!("'{l}'"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        backends_py = backends_py,
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
    let tex_path = paper_dir.join("commit-time-figure.tex");
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
            eprintln!("  warning: commit-time-figure: {e:#}");
            Some(format!("paper/{plot_pdf_name}"))
        }
    };

    Ok(Artifact {
        group: None,
        title: "Commit + status time per operation".to_string(),
        preferred: true,
        tex_path: format!("paper/{}", tex_path.file_name().unwrap().to_string_lossy()),
        pdf_path: preview_pdf,
        tex_abs: tex_path,
        plot_pdfs: vec![pdf_path],
    })
}
