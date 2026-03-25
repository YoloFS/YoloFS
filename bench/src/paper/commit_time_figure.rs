//! Publication figure: commit time per operation (session microbenchmarks).

use super::Artifact;
use crate::report;
use crate::BenchResults;
use anyhow::{Context, Result};
use std::path::Path;

const CAPTION: &str =
    "Commit cost per file operation (\\textmu s/op) for 10\\,000 files. \
     TODO.";
const LABEL: &str = "fig:commit-time";

const WORKLOADS: &[(&str, &str)] = &[
    ("write-files", "create"),
    ("overwrite-files", "overwrite"),
    ("rename-files", "rename"),
    ("unlink-files", "unlink"),
];

/// Backends to show (no native — it has no commit; no agfs-no-perm).
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

    // Collect data: op,backend,commit_us
    let mut data_lines = Vec::new();
    for &(wl_name, op_label) in WORKLOADS {
        for &(backend_key, backend_label) in BACKENDS {
            let commit_us = results
                .workloads
                .iter()
                .find(|w| report::normalize_legacy_workload_name(&w.workload) == wl_name)
                .and_then(|wl| wl.backends.iter().find(|b| b.backend == backend_key))
                .and_then(|b| {
                    let runs = &b.iterations;
                    if runs.is_empty() {
                        return None;
                    }
                    let total_commit: u64 =
                        runs.iter().map(|r| r.commit_ms.unwrap_or(0)).sum();
                    let avg_commit = total_commit as f64 / runs.len() as f64;
                    Some(avg_commit / FILE_COUNT * 1000.0)
                })
                .unwrap_or(0.0);

            data_lines.push(format!("{op_label},{backend_label},{commit_us:.2}"));
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
op,backend,commit_us
{data}
"""

reader = csv.DictReader(StringIO(DATA.strip()))
rows = list(reader)

ops = {ops_py}
backends = [{backends_py}]

# Build lookup: (op, backend) -> commit_us
lookup = {{}}
for r in rows:
    lookup[(r['op'], r['backend'])] = float(r['commit_us'])

backend_colors = {{
    'AgFS':       S.TABLEAU10['blue'],
    'OverlayFS':  S.TABLEAU10['green'],
    'BranchFS':   S.TABLEAU10['orange'],
    'Native':     S.TABLEAU10['gray'],
}}

nb = len(backends)
bar_width = 0.8 / nb

# Size at final print dimensions so LaTeX doesn't scale.
# acmart sigplan column = 3.33in; half = 1.67in.
plt.rcParams.update({{'font.size': 7, 'axes.labelsize': 7, 'xtick.labelsize': 6.5,
                      'ytick.labelsize': 6.5, 'legend.fontsize': 6}})
fig, ax = plt.subplots(figsize=(1.67, 1.5))
x = np.arange(len(ops))

for bi, b in enumerate(backends):
    vals = [lookup.get((op, b), 0) for op in ops]
    offset = (bi - (nb - 1) / 2) * bar_width
    bars = ax.bar(x + offset, vals, bar_width * 0.9,
                  color=backend_colors.get(b, '#999'),
                  edgecolor='white', linewidth=0.5,
                  label=b)

ax.set_xticks(x)
ax.set_xticklabels(ops, fontweight='bold', rotation=45, ha='right')
ax.tick_params(axis='x', length=0)
ax.set_ylabel('commit time (\u00b5s/op)')
ax.set_ylim(bottom=0)
ax.legend(loc='lower center', bbox_to_anchor=(0.5, 1.0), ncol=nb,
          handlelength=1, handletextpad=0.4, borderpad=0.2, columnspacing=0.8)

fig.tight_layout(pad=0.3)
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
    std::fs::write(&tex_path, &tex)
        .with_context(|| format!("writing {}", tex_path.display()))?;

    let preview_pdf = match super::run_pdflatex_cropped(&tex_path, paper_dir) {
        Ok(p) => Some(format!("paper/{}", p.file_name().unwrap().to_string_lossy())),
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
