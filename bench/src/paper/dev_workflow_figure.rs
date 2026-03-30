//! Publication figure: developer workflow phase breakdown.

use super::Artifact;
use crate::BenchResults;
use anyhow::{Context, Result};
use std::path::Path;

const CAPTION: &str =
    "A developer workload of setting up and iterating on the Linux kernel codebase.";
const LABEL: &str = "fig:dev-workflow";
const WORKLOAD: &str = "dev-workflow";

const FACETS: &[(&str, &[&str])] = &[
    ("Worktree", &["worktree", "checkpoint-worktree"]),
    (
        "Init. Build",
        &[
            "config",
            "checkpoint-config",
            "initial-build",
            "checkpoint-initial-build",
        ],
    ),
    ("Read", &["search", "read"]),
    ("Edit", &["edit", "checkpoint-edit"]),
    (
        "Incr. Build",
        &["incremental-build", "checkpoint-incremental-build"],
    ),
    (
        "Git",
        &[
            "git-status",
            "git-diff",
            "git-add",
            "git-commit",
            "checkpoint-git-commit",
        ],
    ),
];

const BACKENDS: &[(&str, &str)] = &[("agfs-realistic", "AgFS"), ("overlayfs", "OverlayFS")];

pub fn artifact_meta(paper_dir: &Path) -> Artifact {
    let tex_path = paper_dir.join("dev-workflow-figure.tex");
    let plot_pdf = paper_dir.join("dev-workflow-figure-plot.pdf");
    Artifact {
        group: None,
        title: "Developer workflow breakdown".to_string(),
        preferred: true,
        tex_path: format!("paper/{}", tex_path.file_name().unwrap().to_string_lossy()),
        pdf_path: None,
        tex_abs: tex_path,
        plot_pdfs: vec![plot_pdf],
    }
}

pub fn render(results: &BenchResults, paper_dir: &Path) -> Result<Artifact> {
    let wl = results
        .workloads
        .iter()
        .find(|w| crate::report::normalize_legacy_workload_name(&w.workload) == WORKLOAD)
        .with_context(|| format!("{WORKLOAD} not found in results"))?;

    let native = wl
        .backends
        .iter()
        .find(|b| b.backend == "native")
        .with_context(|| format!("native backend missing for {WORKLOAD}"))?;

    let mut csv_lines = Vec::new();
    csv_lines.push(
        "facet,backend,run_s,checkpoint_s,native_run_s,run_total_s,checkpoint_total_s,commit_s,native_total_s"
            .to_string(),
    );

    for &(facet_label, categories) in FACETS {
        let native_run_ms = sum_categories(native, categories, false);
        for &(backend_key, backend_label) in BACKENDS {
            let Some(backend) = wl.backends.iter().find(|b| b.backend == backend_key) else {
                continue;
            };
            let run_s = sum_categories(backend, categories, false) / 1000.0;
            let checkpoint_s = sum_categories(backend, categories, true) / 1000.0;
            let run_total_s = (backend.mean_init_ms.unwrap_or(0.0)
                + backend.mean_staging_ms.unwrap_or(backend.mean_total_ms))
                / 1000.0;
            let checkpoint_total_s = sum_categories(
                backend,
                &[
                    "checkpoint-worktree",
                    "checkpoint-config",
                    "checkpoint-initial-build",
                    "checkpoint-edit",
                    "checkpoint-incremental-build",
                    "checkpoint-git-commit",
                ],
                true,
            ) / 1000.0;
            let native_total_s = (native.mean_init_ms.unwrap_or(0.0)
                + native.mean_staging_ms.unwrap_or(native.mean_total_ms))
                / 1000.0;
            csv_lines.push(format!(
                "{},{},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3}",
                facet_label,
                backend_label,
                run_s,
                checkpoint_s,
                native_run_ms / 1000.0,
                run_total_s,
                checkpoint_total_s,
                backend.mean_commit_ms.unwrap_or(0.0) / 1000.0,
                native_total_s,
            ));
        }
    }

    let preamble = super::util::plot_preamble();
    let py_path = paper_dir.join("dev-workflow-figure.py");
    let pdf_path = paper_dir.join("dev-workflow-figure-plot.pdf");
    super::util::ensure_plot_style(paper_dir)?;

    let script = format!(
        r#"{preamble}
from io import StringIO
import csv

DATA = """\
facet,backend,run_s,checkpoint_s,native_run_s,run_total_s,checkpoint_total_s,commit_s,native_total_s
{data}
"""

rows = list(csv.DictReader(StringIO(DATA.strip())))
facets = {facets}
backends = {backends}
colors = {{
    'AgFS': S.BACKEND_COLORS['AgFS'],
    'OverlayFS': S.BACKEND_COLORS['OverlayFS'],
}}
native_line_kw = dict(S.NATIVE_LINE_KW)
native_line_kw['path_effects'] = [pe.withStroke(linewidth=1.8, foreground='white', alpha=0.45)]
native_handle = S.native_legend_handle('Base')
native_handle.set_path_effects(native_line_kw['path_effects'])

plt.rcParams.update({{'font.size': 6.5, 'axes.labelsize': 6.5, 'xtick.labelsize': 6,
                      'ytick.labelsize': 5.2, 'legend.fontsize': 5.8}})

fig = plt.figure(figsize=(2.85, 1.82))
gs = fig.add_gridspec(2, 4, width_ratios=[0.56, 0.56, 0.56, 0.82], wspace=0.42, hspace=0.42)
axes = [
    fig.add_subplot(gs[0, 0]),
    fig.add_subplot(gs[0, 1]),
    fig.add_subplot(gs[0, 2]),
    fig.add_subplot(gs[1, 0]),
    fig.add_subplot(gs[1, 1]),
    fig.add_subplot(gs[1, 2]),
]
ax_total = fig.add_subplot(gs[:, 3])

def facet_rows(name):
    return [r for r in rows if r['facet'] == name]

for idx, facet in enumerate(facets):
    ax = axes[idx]
    fr = facet_rows(facet)
    x = np.array([0.0, 0.18])
    run = [next((float(r['run_s']) for r in fr if r['backend'] == b), 0.0) for b in backends]
    chk = [next((float(r['checkpoint_s']) for r in fr if r['backend'] == b), 0.0) for b in backends]
    native_run = next((float(r['native_run_s']) for r in fr), 0.0)
    for i, backend in enumerate(backends):
        color = colors[backend]
        ax.bar(x[i], run[i], color=color, edgecolor=color, linewidth=0.6, width=0.14)
        ax.bar(x[i], chk[i], bottom=run[i], color='white', edgecolor=color, linewidth=0.6,
               width=0.14)
        ax.bar(x[i], chk[i], bottom=run[i], color='none', edgecolor=color, linewidth=0.0,
               hatch='////', width=0.14, zorder=3)
    ax.axhline(native_run, **native_line_kw)
    ax.set_xticks([])
    ymax = max(max((r + c) for r, c in zip(run, chk)), native_run, 0.0)
    ax.set_ylim(0, ymax * 1.08 if ymax > 0 else 1.0)
    ax.set_xlim(-0.14, 0.32)
    ax.yaxis.set_major_locator(MaxNLocator(nbins=3))
    ax.text(0.5, -0.095, facet, transform=ax.transAxes, ha='center', va='top',
            fontsize=6.2, fontweight='bold')

ax = ax_total
x = np.array([0.0, 0.2])
run_total = [next((float(r['run_total_s']) for r in rows if r['facet'] == facets[0] and r['backend'] == b), 0.0) for b in backends]
commit = [next((float(r['commit_s']) for r in rows if r['facet'] == facets[0] and r['backend'] == b), 0.0) for b in backends]
checkpoint_total = [next((float(r['checkpoint_total_s']) for r in rows if r['facet'] == facets[0] and r['backend'] == b), 0.0) for b in backends]
native_total = next((float(r['native_total_s']) for r in rows if r['facet'] == facets[0]), 0.0)
stack_base = np.array(run_total) + np.array(checkpoint_total)
for i, backend in enumerate(backends):
    color = colors[backend]
    ax.bar(x[i], run_total[i], color=color, edgecolor=color, linewidth=0.6, width=0.14)
    ax.bar(x[i], checkpoint_total[i], bottom=run_total[i], color='white', edgecolor=color, linewidth=0.6,
           width=0.14)
    ax.bar(x[i], checkpoint_total[i], bottom=run_total[i], color='none', edgecolor=color, linewidth=0.0,
           hatch='////', width=0.14, zorder=3)
    ax.bar(x[i], commit[i], bottom=stack_base[i], color='white', edgecolor=color, linewidth=0.6,
           width=0.14)
    ax.bar(x[i], commit[i], bottom=stack_base[i], color='none', edgecolor=color, linewidth=0.0,
           hatch='....', width=0.14, zorder=3)
ax.axhline(native_total, **native_line_kw)
ax.set_xticks([])
ax.set_ylim(bottom=0)
ax.set_xlim(-0.14, 0.34)
ax.yaxis.set_major_locator(MaxNLocator(nbins=4))

for ax in axes + [ax_total]:
    ax.tick_params(axis='x', length=0)
    ax.tick_params(axis='y', length=2, pad=1)
    ax.yaxis.set_major_formatter(plt.FuncFormatter(lambda y, _: '0' if abs(y) < 1e-12 else f'{{y:.2g}}'))

legend_handles = [
    S.backend_legend_handle('AgFS'),
    S.backend_legend_handle('OverlayFS'),
    mpatches.Patch(facecolor='#666', edgecolor='#666', label='run'),
    mpatches.Patch(facecolor='white', edgecolor='#666', hatch='////', label='snapshot'),
    mpatches.Patch(facecolor='white', edgecolor='#666', hatch='....', label='commit'),
    native_handle,
]
fig.legend(handles=legend_handles, loc='upper center', bbox_to_anchor=(0.5, 0.905),
           ncol=6, handlelength=1.1, handletextpad=0.35, borderpad=0.15, columnspacing=0.55)
fig.text(0.055, 0.46, 'time (s)', rotation=90, va='center', ha='center', fontsize=6.5)
fig.subplots_adjust(left=0.12, right=0.99, top=0.8, bottom=0.125)

edit_bbox = axes[3].get_position()
title_y = edit_bbox.y0 - 0.095 * edit_bbox.height
total_bbox = ax_total.get_position()
fig.text(total_bbox.x0 + total_bbox.width / 2, title_y, 'Total', ha='center', va='top',
         fontsize=6.2, fontweight='bold')

fig.savefig('{pdf_path}', bbox_inches='tight', dpi=300)
plt.close(fig)
"#,
        preamble = preamble,
        data = csv_lines.join("\n"),
        facets = format!(
            "[{}]",
            FACETS
                .iter()
                .map(|(name, _)| format!("'{}'", name))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        backends = format!(
            "[{}]",
            BACKENDS
                .iter()
                .map(|(_, label)| format!("'{}'", label))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        pdf_path = pdf_path.display(),
    );

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
    let tex_path = paper_dir.join("dev-workflow-figure.tex");
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
         \\includegraphics[width=\\columnwidth]{{{plot_pdf_name}}}\n\
         \\caption{{{CAPTION}}}\n\
         \\label{{{LABEL}}}\n\
         \\end{{figure}}\n\
         % --- END figure fragment ---\n\
         \\end{{document}}\n"
    );
    std::fs::write(&tex_path, &tex).with_context(|| format!("writing {}", tex_path.display()))?;

    let preview_pdf = match super::run_pdflatex_cropped(&tex_path, paper_dir) {
        Ok(p) => Some(format!(
            "paper/{}",
            p.file_name().unwrap().to_string_lossy()
        )),
        Err(e) => {
            eprintln!("  warning: dev-workflow-figure: {e:#}");
            Some(format!("paper/{plot_pdf_name}"))
        }
    };

    Ok(Artifact {
        group: None,
        title: "Developer workflow breakdown".to_string(),
        preferred: true,
        tex_path: format!("paper/{}", tex_path.file_name().unwrap().to_string_lossy()),
        pdf_path: preview_pdf,
        tex_abs: tex_path,
        plot_pdfs: vec![pdf_path],
    })
}

fn sum_categories(backend: &crate::BackendResult, categories: &[&str], checkpoints: bool) -> f64 {
    let Some(series) = &backend.macro_step_series else {
        return 0.0;
    };
    series
        .steps
        .iter()
        .filter(|step| {
            let Some(category) = crate::report::dev_workflow_step_category(&step.step) else {
                return false;
            };
            categories.contains(&category) && category.starts_with("checkpoint-") == checkpoints
        })
        .map(|step| step.ms as f64)
        .sum()
}
