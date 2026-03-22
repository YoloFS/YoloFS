//! Publication figure: metadata operation latency small multiples.
//!
//! Layout: 2 rows (100 files, 10K files) × 7 columns (ops).
//! Within each subplot, x-axis = source (base, stage, checkpoint),
//! bars colored by backend. Legend identifies backends.
//!
//! Multiple figure variants are generated to allow comparison.

use super::util::backend_display_name;
use super::Artifact;
use crate::report;
use crate::BenchResults;
use anyhow::{Context, Result};
use std::path::Path;


/// Native is drawn as a horizontal reference line, not a bar.
const NATIVE: &str = "native";

/// Backends drawn as bars (display order).
const BAR_BACKENDS: &[&str] = &[
    "agfs-no-perm",
    "agfs-realistic",
    "overlayfs",
    "branchfs",
];

/// All backends emitted into the CSV (native + bar backends).
const ALL_BACKENDS: &[&str] = &[
    "native",
    "agfs-no-perm",
    "agfs-realistic",
    "overlayfs",
    "branchfs",
];

/// Display names for figure legend.
fn fig_backend_name(key: &str) -> &'static str {
    match key {
        "native" => "Native",
        "agfs-no-perm" => "AgFS (no perm)",
        "agfs-realistic" => "AgFS",
        "overlayfs" => "OverlayFS",
        "branchfs" => "BranchFS",
        _ => backend_display_name(key),
    }
}

/// Operations and their workload name stems.
const OPS: &[(&str, &str)] = &[
    ("create", "meta-create"),
    ("open", "meta-open"),
    ("stat", "meta-stat"),
    ("readdir", "meta-readdir"),
    ("append", "meta-append"),
    ("rename", "meta-rename"),
    ("unlink", "meta-unlink"),
];

/// Source variants in display order.
const SOURCES: &[(&str, &str)] = &[("base", "Base"), ("checkpoint", "Chkpt"), ("stage", "Stage")];

// ── Figure variant configuration ────────────────────────────────────────────

#[derive(Clone, Copy)]
enum OutlierStrategy {
    /// Use brokenaxes to show both ranges.
    BrokenAxis { break_threshold: f64, height_ratios: (u32, u32) },
    /// Cap outlier bars with hatching and text annotations.
    CapAndAnnotate { cap_factor: f64 },
    /// Plain auto-scaled axes, no special treatment.
    Plain,
}

struct FigureVariant {
    /// Output filename stem (e.g. "meta-ops-broken").
    name: &'static str,
    /// Human-readable title for the HTML index.
    title: &'static str,
    /// The preferred variant is shown expanded; others are collapsed.
    preferred: bool,
    strategy: OutlierStrategy,
}

/// Shared caption and label — only one variant ends up in the paper.
const CAPTION: &str =
    "Metadata operation latency (\\textmu s). \
     The dashed line marks the native ext4 baseline. \
     Bars grouped by file source layer (base / checkpoint / staged). TODO";
const LABEL: &str = "fig:meta-ops";

const VARIANTS: &[FigureVariant] = &[
    FigureVariant {
        name: "meta-ops-broken",
        title: "Meta ops (broken axis)",
        preferred: false,
        strategy: OutlierStrategy::BrokenAxis {
            break_threshold: 3.0,
            height_ratios: (1, 5),
        },
    },
    FigureVariant {
        name: "meta-ops-capped",
        title: "Meta ops (capped + annotated)",
        preferred: true,
        strategy: OutlierStrategy::CapAndAnnotate { cap_factor: 5.0 },
    },
    FigureVariant {
        name: "meta-ops-plain",
        title: "Meta ops (plain)",
        preferred: false,
        strategy: OutlierStrategy::Plain,
    },
];

// ── Public API ──────────────────────────────────────────────────────────────

pub fn render(results: &BenchResults, paper_dir: &Path) -> Result<Vec<Artifact>> {
    let data_csv = build_data_csv(results);
    let mut artifacts = Vec::new();

    for variant in VARIANTS {
        match render_variant(variant, &data_csv, paper_dir) {
            Ok(art) => artifacts.push(art),
            Err(e) => eprintln!("  warning: {}: {e:#}", variant.name),
        }
    }

    Ok(artifacts)
}

/// Return artifact metadata for all variants without rendering (for install-paper).
pub fn artifact_metas(paper_dir: &Path) -> Vec<Artifact> {
    VARIANTS
        .iter()
        .map(|v| {
            let tex_path = paper_dir.join(format!("{}.tex", v.name));
            let plot_pdf = paper_dir.join(format!("{}-plot.pdf", v.name));
            Artifact {
                group: Some("Metadata operation latency".to_string()),
                title: v.title.to_string(),
                preferred: v.preferred,
                tex_path: format!("paper/{}", tex_path.file_name().unwrap().to_string_lossy()),
                pdf_path: None,
                tex_abs: tex_path,
                plot_pdfs: vec![plot_pdf],
            }
        })
        .collect()
}

fn render_variant(
    variant: &FigureVariant,
    data_csv: &str,
    paper_dir: &Path,
) -> Result<Artifact> {
    let py_path = paper_dir.join(format!("{}.py", variant.name));
    let pdf_path = paper_dir.join(format!("{}-plot.pdf", variant.name));

    super::util::ensure_plot_style(paper_dir)?;
    let script = build_script(variant, data_csv, &pdf_path);
    std::fs::write(&py_path, &script)
        .with_context(|| format!("writing {}", py_path.display()))?;

    let out = std::process::Command::new("python3")
        .arg(&py_path)
        .output()
        .with_context(|| format!("running {}", py_path.display()))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("matplotlib script failed: {stderr}");
    }

    // Write a full LaTeX document that wraps the matplotlib PDF in a
    // captioned figure float, then compile + crop for preview.
    let plot_pdf_name = pdf_path.file_name().unwrap().to_string_lossy();
    let tex_path = paper_dir.join(format!("{}.tex", variant.name));
    let tex = format!(
        "\\PassOptionsToPackage{{activate=false}}{{microtype}}\n\
         \\documentclass[sigplan,screen]{{acmart}}\n\
         \\settopmatter{{printacmref=false,printfolios=false}}\n\
         \\renewcommand\\footnotetextcopyrightpermission[1]{{}}\n\
         \\usepackage{{graphicx}}\n\
         \\begin{{document}}\n\
         \\thispagestyle{{empty}}\n\
         % --- BEGIN figure fragment (includable via \\input) ---\n\
         \\begin{{figure*}}[h]\n\
         \\centering\n\
         \\includegraphics[width=\\textwidth]{{{plot_pdf_name}}}\n\
         \\caption{{{CAPTION}}}\n\
         \\label{{{LABEL}}}\n\
         \\end{{figure*}}\n\
         % --- END figure fragment ---\n\
         \\end{{document}}\n",
    );
    std::fs::write(&tex_path, &tex)
        .with_context(|| format!("writing {}", tex_path.display()))?;

    let preview_pdf = match super::run_pdflatex_cropped(&tex_path, paper_dir) {
        Ok(p) => Some(format!("paper/{}", p.file_name().unwrap().to_string_lossy())),
        Err(e) => {
            eprintln!("  warning: {}: {e:#}", variant.name);
            // Fall back to the raw matplotlib PDF.
            Some(format!("paper/{plot_pdf_name}"))
        }
    };

    Ok(Artifact {
        group: Some("Metadata operation latency".to_string()),
        title: variant.title.to_string(),
        preferred: variant.preferred,
        tex_path: format!("paper/{}", tex_path.file_name().unwrap().to_string_lossy()),
        pdf_path: preview_pdf,
        tex_abs: tex_path.to_path_buf(),
        plot_pdfs: vec![pdf_path.to_path_buf()],
    })
}

// ── Data collection (shared across all variants) ────────────────────────────

fn build_data_csv(results: &BenchResults) -> String {
    let mut lines = Vec::new();
    lines.push("op,size,source,backend,lat_us".to_string());

    for &(op_label, stem) in OPS {
        for &(size, size_suffix) in &[(100, "-100"), (10000, "")] {
            let sources: &[&str] = if stem == "meta-create" {
                &["stage"]
            } else {
                &["base", "stage", "checkpoint"]
            };

            for &source in sources {
                let wl_name = if stem == "meta-create" {
                    if size == 100 {
                        "meta-create-100".to_string()
                    } else {
                        "meta-create".to_string()
                    }
                } else {
                    format!("{stem}{size_suffix}-{source}")
                };

                for &backend in ALL_BACKENDS {
                    let lat_us = results
                        .workloads
                        .iter()
                        .find(|w| {
                            report::normalize_legacy_workload_name(&w.workload) == wl_name
                        })
                        .and_then(|wl| {
                            wl.backends
                                .iter()
                                .find(|b| b.backend == backend)
                                .and_then(|b| b.mean_iops)
                                .map(|iops| 1_000_000.0 / iops)
                        });

                    if let Some(lat) = lat_us {
                        lines.push(format!(
                            "{op_label},{size},{source},{},{lat:.2}",
                            fig_backend_name(backend)
                        ));
                    }
                }
            }
        }
    }

    lines.join("\n")
}

// ── Python script generation ────────────────────────────────────────────────

fn build_script(variant: &FigureVariant, data_csv: &str, pdf_path: &Path) -> String {
    let preamble = super::util::plot_preamble();
    let ops_py: Vec<String> = OPS.iter().map(|(l, _)| format!("'{l}'")).collect();
    let bar_backends_py: Vec<String> = BAR_BACKENDS
        .iter()
        .map(|b| format!("'{}'", fig_backend_name(b)))
        .collect();
    let sources_py: Vec<String> = SOURCES.iter().map(|(_, s)| format!("'{s}'")).collect();
    let native_name = fig_backend_name(NATIVE);

    let strategy_code = match variant.strategy {
        OutlierStrategy::BrokenAxis {
            break_threshold,
            height_ratios,
        } => format!(
            "STRATEGY = 'broken'\nBREAK_THRESHOLD = {break_threshold}\n\
             HEIGHT_RATIOS = ({}, {})\n",
            height_ratios.0, height_ratios.1
        ),
        OutlierStrategy::CapAndAnnotate { cap_factor } => {
            format!("STRATEGY = 'capped'\nCAP_FACTOR = {cap_factor}\n")
        }
        OutlierStrategy::Plain => "STRATEGY = 'plain'\n".to_string(),
    };

    format!(
        r#"{preamble}
DATA = """\
{data_csv}
"""

reader = csv.DictReader(StringIO(DATA.strip()))
rows = list(reader)

ops = [{ops}]
bar_backends = [{bar_backends}]
native_key = '{native_key}'
sources = [{sources}]
source_full = {{'Base': 'base', 'Chkpt': 'checkpoint', 'Stage': 'stage'}}
sizes = [100, 10000]
size_labels = {{100: '100 files', 10000: '10K files'}}

{strategy_code}

# Build lookup: (op, size, source, backend) -> lat_us
lookup = {{}}
for r in rows:
    key = (r['op'].strip(), int(r['size'].strip()), r['source'].strip(), r['backend'].strip())
    lookup[key] = float(r['lat_us'].strip())

nb = len(bar_backends)
bar_width = 0.8 / nb

def find_break(vals, floor_vals):
    """Find break point. floor_vals must be fully visible (not in upper segment)."""
    sv = sorted(set(v for v in vals if v > 0))
    if len(sv) < 2:
        return None
    best_ratio, best_idx = 1.0, -1
    for i in range(len(sv) - 1):
        ratio = sv[i+1] / sv[i]
        if ratio > best_ratio:
            best_ratio, best_idx = ratio, i
    if best_ratio < BREAK_THRESHOLD:
        return None
    lo_max = sv[best_idx] * 1.3
    floor_max = max((v for v in floor_vals if v > 0), default=0)
    if floor_max > lo_max:
        return None
    return ((0, lo_max), (sv[best_idx + 1] * 0.85, sv[-1] * 1.15))

def compute_cap(vals, floor_vals):
    """Compute cap, ensuring floor_vals are never capped."""
    sv = sorted(v for v in vals if v > 0)
    if len(sv) < 2:
        return sv[-1] * 1.35 if sv else 1.0
    reasonable = sv[:]
    while len(reasonable) > 2:
        ref_idx = max(0, int(len(reasonable) * 0.6) - 1)
        if reasonable[-1] > reasonable[ref_idx] * CAP_FACTOR:
            reasonable.pop()
        else:
            break
    cap = reasonable[-1] * 1.35
    floor_max = max((v for v in floor_vals if v > 0), default=0)
    if floor_max > 0:
        cap = max(cap, floor_max * 1.15)
    return cap

# ── Figure setup ──
ncols = len(ops)
nrows = 2

if STRATEGY == 'broken':
    from brokenaxes import brokenaxes

fig = plt.figure(figsize=(14, 4.5))
gs = gridspec.GridSpec(nrows, ncols, figure=fig, wspace=0.35, hspace=0.35)

drew_native_line = False

for row_idx, size in enumerate(sizes):
    for col_idx, op in enumerate(ops):
        if op == 'create':
            src_labels = ['\u2014']
            src_keys = ['stage']
        else:
            src_labels = list(sources)
            src_keys = [source_full[s] for s in sources]

        ns = len(src_labels)
        x = np.arange(ns)

        all_vals = []
        floor_vals = []
        for sk in src_keys:
            for b in [native_key] + bar_backends:
                v = lookup.get((op, size, sk, b), 0)
                if v > 0:
                    all_vals.append(v)
                    if b in S.UNCAPPABLE:
                        floor_vals.append(v)
        if not all_vals:
            continue

        # ── Create axis depending on strategy ──
        is_broken = False
        cap = None

        if STRATEGY == 'broken':
            brk = find_break(all_vals, floor_vals)
            if brk is not None:
                ax = brokenaxes(ylims=brk, subplot_spec=gs[row_idx, col_idx],
                                height_ratios=HEIGHT_RATIOS, d=0.008, tilt=45,
                                despine=False)
                is_broken = True
            else:
                ax = fig.add_subplot(gs[row_idx, col_idx])
                ax.set_ylim(0, max(all_vals) * 1.15)
        elif STRATEGY == 'capped':
            ax = fig.add_subplot(gs[row_idx, col_idx])
            cap = compute_cap(all_vals, floor_vals)
            ax.set_ylim(0, cap)
        else:
            ax = fig.add_subplot(gs[row_idx, col_idx])
            ax.set_ylim(0, max(all_vals) * 1.15)

        if not is_broken:
            ax.spines['top'].set_visible(False)
            ax.spines['right'].set_visible(False)

        # ── Draw Native baseline ──
        native_src = 'base' if op != 'create' else 'stage'
        nv = lookup.get((op, size, native_src, native_key), 0)
        if nv > 0:
            ax.axhline(y=nv, **S.NATIVE_LINE_KW,
                       label=native_key if not drew_native_line else None)
            if not drew_native_line:
                drew_native_line = True

        # ── Draw bars ──
        annotations = []
        capped_at_src = {{}}
        for bi, b in enumerate(bar_backends):
            vals = [lookup.get((op, size, sk, b), 0) for sk in src_keys]
            offset = (bi - (nb - 1) / 2) * bar_width

            if cap is not None:
                display = [min(v, cap) if v > 0 else 0 for v in vals]
            else:
                display = vals

            bars = ax.bar(x + offset, display, bar_width * 0.9,
                          color=S.BACKEND_COLORS[b],
                          edgecolor='white', linewidth=0.3,
                          label=b if row_idx == 0 and col_idx == 0 else None)

            # Capped bar decorations: fade top to white and annotate.
            if cap is not None:
                bw = bar_width * 0.9
                for i, v in enumerate(vals):
                    if v > cap:
                        fade_bottom = cap * 0.65
                        fade_height = cap - fade_bottom
                        xpos = x[i] + offset
                        n_steps = 80
                        step_h = fade_height / n_steps
                        for s in range(n_steps):
                            alpha = (s / (n_steps - 1)) * 0.92
                            y_bot = fade_bottom + s * step_h
                            ax.bar(xpos, step_h, bw, bottom=y_bot,
                                   color=(1, 1, 1, alpha), edgecolor='none',
                                   zorder=6)
                        capped_at_src.setdefault(i, []).append((v, xpos, b))

        # Ranked annotations for capped bars.
        if cap is not None:
            all_capped = []
            for si, entries in capped_at_src.items():
                all_capped.extend(entries)
            if all_capped:
                ranked = sorted(all_capped, key=lambda e: e[0], reverse=True)
                for rank, (v, xp, _b) in enumerate(ranked):
                    yp = cap * (0.95 - rank * 0.05)
                    annotations.append((xp, yp, S.fmt_lat(v)))

        for (xp, yp, txt) in annotations:
            ax.annotate(txt, (xp, yp), fontsize=9, ha='center', va='center',
                        color='#b00', fontweight='bold', zorder=7,
                        bbox=dict(boxstyle='round,pad=0.15', fc='white',
                                  ec='none', alpha=0.7))

        # ── Op name below bottom row ──
        if row_idx == nrows - 1:
            if is_broken:
                ax.axs[-1].set_xlabel(op, fontweight='bold', labelpad=4)
            else:
                ax.set_xlabel(op, fontweight='bold', labelpad=4)

        # ── Ticks and labels ──
        if is_broken:
            for a in ax.axs:
                a.yaxis.set_major_locator(MaxNLocator(nbins=4, integer=True))
                a.tick_params(axis='x', length=0, pad=2)
                a.set_xticks(x)
                a.set_xticklabels([])
            ax.axs[-1].set_xticklabels(src_labels)
            if col_idx == 0:
                ax.set_ylabel(size_labels[size] + '\nlatency (\u00b5s)',
                              labelpad=25)
        else:
            ax.set_xticks(x)
            ax.set_xticklabels(src_labels)
            ax.tick_params(axis='x', length=0, pad=2)
            if col_idx == 0:
                ax.set_ylabel(size_labels[size] + '\nlatency (\u00b5s)')

# ── Legend ──
legend_items = [S.native_legend_handle(native_key)]
for b in bar_backends:
    legend_items.append(S.backend_legend_handle(b))

fig.legend(handles=legend_items, loc='upper center', ncol=nb + 1,
           bbox_to_anchor=(0.5, 1.0))

fig.savefig('{pdf_path}', bbox_inches='tight', dpi=300)
plt.close(fig)
"#,
        preamble = preamble,
        data_csv = data_csv,
        ops = ops_py.join(", "),
        bar_backends = bar_backends_py.join(", "),
        native_key = native_name,
        sources = sources_py.join(", "),
        strategy_code = strategy_code,
        pdf_path = pdf_path.display(),
    )
}
