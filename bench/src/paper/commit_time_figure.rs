//! Publication figure: commit time per operation (session microbenchmarks).

use super::Artifact;
use crate::report;
use crate::BenchResults;
use anyhow::{Context, Result};
use std::path::Path;

const CAPTION: &str = "Commit cost per file operation (\\textmu s/op) for 10\\,000 files. TODO.";
const LABEL: &str = "fig:commit-time";

const WORKLOADS: &[(&str, &str)] = &[
    ("write-files", "create"),
    ("overwrite-files", "overwrite"),
    ("rename-files", "rename"),
    ("unlink-files", "unlink"),
];

const NATIVE_BASELINES: &[(&str, &str)] = &[
    ("create", "meta-create"),
    ("overwrite", "meta-append-base"),
    ("rename", "meta-rename-base"),
    ("unlink", "meta-unlink-base"),
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
        title: "Commit time per operation".to_string(),
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
            }
        }
    }

    let mut baseline_lines = Vec::new();
    for &(op_label, wl_name) in NATIVE_BASELINES {
        let wl = results
            .workloads
            .iter()
            .find(|w| report::normalize_legacy_workload_name(&w.workload) == wl_name)
            .and_then(|wl| wl.backends.iter().find(|b| b.backend == "native"));

        if let Some(b) = wl {
            let runs = &b.iterations;
            if runs.is_empty() {
                continue;
            }
            let avg_total_ms =
                runs.iter().map(|r| r.total_ms as f64).sum::<f64>() / runs.len() as f64;
            baseline_lines.push(format!(
                "{op_label},{:.2}",
                avg_total_ms / FILE_COUNT * 1000.0
            ));
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

BASELINE = """\
op,us_per_op
{baseline_data}
"""

reader = csv.DictReader(StringIO(DATA.strip()))
rows = list(reader)

baseline = {{}}
for r in csv.DictReader(StringIO(BASELINE.strip())):
    baseline[r['op']] = float(r['us_per_op'])

ops = {ops_py}
backends = [{backends_py}]

# Build lookup: (op, backend, metric) -> us_per_op
lookup = {{}}
for r in rows:
    lookup[(r['op'], r['backend'], r['metric'])] = float(r['us_per_op'])

backend_colors = {{
    'AgFS':       S.TABLEAU10['blue'],
    'OverlayFS':  S.TABLEAU10['green'],
    'BranchFS':   S.TABLEAU10['orange'],
}}

plt.rcParams.update({{'font.size': 8.6, 'axes.labelsize': 8.6, 'xtick.labelsize': 7.8,
                      'ytick.labelsize': 7.8, 'legend.fontsize': 7.2}})

nb = len(backends)
bar_height = 0.5 / nb
y = np.arange(len(ops)) * 0.62

# Compute commit panel range.
commit_max = max(
    [lookup.get((op, b, 'commit'), 0) for op in ops for b in backends]
    + [baseline.get(op, 0) for op in ops]
) * 1.1

fig, ax_commit = plt.subplots(1, 1, figsize=(2.7, 1.08))

# Left panel: commit time.
for bi, b in enumerate(backends):
    vals = [lookup.get((op, b, 'commit'), 0) for op in ops]
    offset = ((nb - 1) / 2 - bi) * bar_height
    ax_commit.barh(y + offset, vals, bar_height * 0.85,
                   color=backend_colors.get(b, '#999'),
                   edgecolor='white', linewidth=0.3,
                   label=b)
group_half = bar_height * nb * 0.5
for oi, op in enumerate(ops):
    val = baseline.get(op)
    if val is not None:
        ax_commit.vlines(val, y[oi] - group_half, y[oi] + group_half, **S.NATIVE_LINE_KW)
ax_commit.set_xlabel('commit time (\u00b5s/file)')
ax_commit.xaxis.labelpad = 1
ax_commit.set_xlim(left=0)
ax_commit.set_yticks(y)
ax_commit.set_yticklabels([op for op in ops], fontweight='bold')
ax_commit.tick_params(axis='y', length=0, pad=4)

# Set axis range.
from matplotlib.ticker import MaxNLocator
ax_commit.set_xlim(0, commit_max)
ax_commit.xaxis.set_major_locator(MaxNLocator(nbins=6, integer=True))

# Legend at top, shared.
from matplotlib.patches import Patch
legend_items = [Patch(facecolor=backend_colors[b], edgecolor='white', label=b) for b in backends]
legend_items.append(S.native_legend_handle('Base'))
fig.legend(handles=legend_items, loc='upper center', bbox_to_anchor=(0.5, 0.995),
           ncol=nb + 1, handlelength=0.95, handletextpad=0.35,
           borderpad=0.15, columnspacing=0.55)

fig.subplots_adjust(left=0.2, right=0.99, top=0.79, bottom=0.22)

fig.savefig('{pdf_path}', bbox_inches='tight', dpi=300)
plt.close(fig)
"#,
        preamble = preamble,
        data = data_lines.join("\n"),
        baseline_data = baseline_lines.join("\n"),
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
        title: "Commit time per operation".to_string(),
        preferred: true,
        tex_path: format!("paper/{}", tex_path.file_name().unwrap().to_string_lossy()),
        pdf_path: preview_pdf,
        tex_abs: tex_path,
        plot_pdfs: vec![pdf_path],
    })
}
