//! Publication figure: checkpoint depth scaling (split create/read/status/commit latency).

use super::Artifact;
use anyhow::{Context, Result};
use std::path::Path;

const CAPTION: &str = "Snapshot scalability. The latency of creating a new file, reading an existing file, and committing all snapshots back to base as the number of snapshots grow. OverlayFS fails to support more snapshots because of mount option limits.";
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

    // For each backend, find the maximum depth that create or read achieved.
    // Commit and status data beyond that depth is invalid (backend stopped
    // early but still reported a time for the partial build).
    let mut max_depth_per_backend: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for res in &data {
        if res.mode == "commit" || res.mode == "status" {
            continue;
        }
        let max_d = res.points.iter().map(|p| p.depth).max().unwrap_or(0);
        let entry = max_depth_per_backend
            .entry(res.backend.clone())
            .or_insert(0);
        *entry = (*entry).max(max_d);
    }

    let mut create_lines = Vec::new();
    let mut read_lines = Vec::new();
    let mut commit_lines = Vec::new();
    for res in &data {
        if res.backend == "agfs-no-perm" || res.mode == "status" {
            continue;
        }
        // In this figure agfs-realistic is the only AgFS variant, so label it
        // simply "AgFS" rather than "AgFS-R".
        let label = match res.backend.as_str() {
            "agfs-realistic" => "AgFS",
            other => super::util::backend_display_name(other),
        }
        .to_string();
        let depth_cap = max_depth_per_backend
            .get(&res.backend)
            .copied()
            .unwrap_or(usize::MAX);
        for p in &res.points {
            if res.mode == "commit" && p.depth > depth_cap {
                continue;
            }
            let row = format!("{},{},{:.2}", label, p.depth, p.mean_us);
            match res.mode.as_str() {
                "create" => create_lines.push(row),
                "read" => read_lines.push(row),
                "commit" => commit_lines.push(row),
                _ => {}
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

COMMIT_DATA = """\
backend,depth,mean_us
{commit_data}
"""

plt.rcParams.update({{'font.size': 8, 'axes.labelsize': 8, 'xtick.labelsize': 7.5,
                      'ytick.labelsize': 7.5, 'legend.fontsize': 7}})

order = ['AgFS', 'OverlayFS', 'BranchFS']
colors = [S.BACKEND_COLORS.get(n, S.TABLEAU10['gray']) for n in order]
overlay_name = 'OverlayFS'
overlay_depths = (
    [int(r['depth']) for r in csv.DictReader(StringIO(CREATE_DATA.strip())) if r['backend'] == overlay_name]
    + [int(r['depth']) for r in csv.DictReader(StringIO(READ_DATA.strip())) if r['backend'] == overlay_name]
    + [int(r['depth']) for r in csv.DictReader(StringIO(COMMIT_DATA.strip())) if r['backend'] == overlay_name]
)
overlay_failure_depth = max(overlay_depths) if overlay_depths else None

fig = plt.figure(figsize=(3.33, 1.3))
# Manual positions: [left, bottom, width, height] in figure coords.
# Equal plot widths (pw), uniform visual gaps between plot edges.
pw = 0.24
h = 0.54
bot = 0.22
left1 = 0.12
gap = 0.045
left2 = left1 + pw + gap
left3 = left2 + pw + gap + 0.04  # extra for commit ylabel
ax_create = fig.add_axes([left1, bot, pw, h])
ax_read   = fig.add_axes([left2, bot, pw, h])
ax_commit = fig.add_axes([left3, bot, pw, h])

def plot_panel(ax, data_csv, panel_label, show_ylabel, ylabel=None, exclude_backends=None,
               mark_overlay_failure=False):
    reader = csv.DictReader(StringIO(data_csv.strip()))
    rows = list(reader)
    for i, name in enumerate(order):
        if exclude_backends and name in exclude_backends:
            continue
        pts = [(int(r['depth']), float(r['mean_us'])) for r in rows if r['backend'] == name]
        if not pts:
            continue
        pts.sort()
        xs = [p[0] for p in pts]
        ys = [p[1] for p in pts]
        ax.plot(xs, ys, marker='o', markersize=2.5, linewidth=1.2,
                color=colors[i], label=name)
        if mark_overlay_failure and name == overlay_name and overlay_failure_depth is not None and xs and xs[-1] == overlay_failure_depth:
            x_cross = xs[-1] + 4
            y_cross = ys[-1]
            ax.plot([x_cross], [y_cross], marker='x', markersize=4.0,
                    markeredgewidth=0.9, linestyle='None', color=colors[i],
                    clip_on=False, zorder=6)
    ax.set_ylim(bottom=0)
    if show_ylabel:
        ax.set_ylabel(ylabel or 'latency (\u00b5s/op)')
    elif show_ylabel is None:
        pass  # tick labels visible, no ylabel text
    else:
        ax.tick_params(axis='y', labelleft=False)
    if overlay_failure_depth is not None:
        ax.set_xlim(right=max(ax.get_xlim()[1], overlay_failure_depth + 8))
    ax.yaxis.set_major_locator(MaxNLocator(nbins=4, min_n_ticks=3,
                                           steps=[1, 2, 2.5, 5, 10]))

plot_panel(ax_create, CREATE_DATA, 'create', show_ylabel=True, mark_overlay_failure=True)
plot_panel(ax_read, READ_DATA, 'read', show_ylabel=False, mark_overlay_failure=True)
# Main commit panel: AgFS and OverlayFS only (linear scale).
plot_panel(ax_commit, COMMIT_DATA, 'commit', show_ylabel=True, ylabel='latency (ms)',
           exclude_backends={{'BranchFS'}}, mark_overlay_failure=True)

# Panel titles.
for ax, name in [(ax_create, 'create'), (ax_read, 'read'), (ax_commit, 'commit')]:
    ax.set_title(name, fontweight='bold', fontsize=7.5, pad=2)

from matplotlib.ticker import FuncFormatter

# Cap commit y-axis to non-BranchFS data range.
commit_reader = csv.DictReader(StringIO(COMMIT_DATA.strip()))
commit_non_branch = [float(r['mean_us']) for r in commit_reader if r['backend'] != 'BranchFS']
if commit_non_branch:
    ax_commit.set_ylim(top=max(commit_non_branch) * 1.3)
ax_commit.yaxis.set_major_locator(MaxNLocator(nbins=4, min_n_ticks=3,
                                              steps=[1, 2, 2.5, 5, 10]))

# Convert commit y-axis from µs to ms for readability.
ax_commit.yaxis.set_major_formatter(FuncFormatter(lambda y, _: f'{{y/1000:.0f}}'))

# Tiny inset in the commit panel showing all backends on BranchFS scale (seconds).
inset = ax_commit.inset_axes([0.58, 0.05, 0.28, 0.35])
commit_reader2 = csv.DictReader(StringIO(COMMIT_DATA.strip()))
all_commit = list(commit_reader2)
for i, name in enumerate(order):
    pts = [(int(r['depth']), float(r['mean_us'])) for r in all_commit if r['backend'] == name]
    if not pts:
        continue
    pts.sort()
    inset.plot([p[0] for p in pts], [p[1] for p in pts],
               marker='o', markersize=1.2, linewidth=0.8, color=colors[i])
    if name == overlay_name and overlay_failure_depth is not None and pts and pts[-1][0] == overlay_failure_depth:
        inset.plot([pts[-1][0] + 4], [pts[-1][1]], marker='x', markersize=2.4,
                   markeredgewidth=0.7, linestyle='None', color=colors[i],
                   clip_on=False, zorder=6)
inset.set_ylim(bottom=0, top=11e6)
inset.set_xticks([])
inset.set_yticks([])
inset.xaxis.set_minor_locator(plt.NullLocator())
inset.yaxis.set_minor_locator(plt.NullLocator())
# Place '10s' outside top of y-axis (left), '100' outside right of x-axis (bottom).
inset.text(-0.05, 0.95, '10s', transform=inset.transAxes, fontsize=7.5,
           ha='right', va='top')
inset.text(1.05, 0.02, '100', transform=inset.transAxes, fontsize=7.5,
           ha='left', va='bottom')
inset.tick_params(labelsize=7, pad=0.5, length=1.5)
inset.tick_params(axis='y', which='major', pad=0.5)
for label in inset.yaxis.get_ticklabels():
    label.set_clip_on(False)
inset.patch.set_facecolor('white')
inset.patch.set_edgecolor('gray')
for sp in inset.spines.values():
    sp.set_linewidth(0.4)
    sp.set_color('gray')


handles, labels = ax_create.get_legend_handles_labels()
fig.legend(handles=handles, labels=labels, loc='upper center',
           bbox_to_anchor=(0.5, 0.97), ncol=len(order),
           handlelength=1.5, handletextpad=0.4,
           borderpad=0.15, columnspacing=0.8)

# Shared x-axis label.
fig.text(0.5, 0.02, 'number of snapshots', ha='center', fontsize=8)

fig.savefig('{pdf_path}', bbox_inches='tight', dpi=300)
plt.close(fig)
"#,
        preamble = preamble,
        create_data = create_lines.join("\n"),
        read_data = read_lines.join("\n"),
        commit_data = commit_lines.join("\n"),
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
