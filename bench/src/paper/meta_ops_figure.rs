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

const CAPTION: &str =
    "Per-operation metadata latency (\\textmu s). \
     Top row: 100-file directory; bottom row: 10\\,000-file directory. \
     Bars grouped by file source layer (base / staged / checkpoint).";
const LABEL: &str = "fig:meta-ops";

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
        "agfs-no-perm" => "AgFS-NP",
        "agfs-realistic" => "AgFS",
        "overlayfs" => "OvlFS",
        "branchfs" => "BrFS",
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

/// Source variants.
const SOURCES: &[(&str, &str)] = &[("base", "B"), ("stage", "S"), ("checkpoint", "C")];

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
    strategy: OutlierStrategy,
}

const VARIANTS: &[FigureVariant] = &[
    FigureVariant {
        name: "meta-ops-broken",
        title: "Meta ops (broken axis)",
        strategy: OutlierStrategy::BrokenAxis {
            break_threshold: 3.0,
            height_ratios: (1, 5),
        },
    },
    FigureVariant {
        name: "meta-ops-capped",
        title: "Meta ops (capped + annotated)",
        strategy: OutlierStrategy::CapAndAnnotate { cap_factor: 5.0 },
    },
    FigureVariant {
        name: "meta-ops-plain",
        title: "Meta ops (plain)",
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

fn render_variant(
    variant: &FigureVariant,
    data_csv: &str,
    paper_dir: &Path,
) -> Result<Artifact> {
    let py_path = paper_dir.join(format!("{}.py", variant.name));
    let pdf_path = paper_dir.join(format!("{}.pdf", variant.name));

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

    let tex_path = paper_dir.join(format!("{}.tex", variant.name));
    let tex = format!(
        "% --- BEGIN figure fragment (includable via \\input) ---\n\
         \\begin{{figure*}}[t]\n\
         \\centering\n\
         \\includegraphics[width=\\textwidth]{{{pdf_name}}}\n\
         \\caption{{{CAPTION}}}\n\
         \\label{{{LABEL}}}\n\
         \\end{{figure*}}\n\
         % --- END figure fragment ---\n",
        pdf_name = pdf_path.file_name().unwrap().to_string_lossy(),
    );
    std::fs::write(&tex_path, &tex)
        .with_context(|| format!("writing {}", tex_path.display()))?;

    Ok(Artifact {
        group: Some("Metadata operation latency".to_string()),
        title: variant.title.to_string(),
        tex_path: format!("paper/{}", tex_path.file_name().unwrap().to_string_lossy()),
        pdf_path: Some(format!(
            "paper/{}",
            pdf_path.file_name().unwrap().to_string_lossy(),
        )),
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
        r#"#!/usr/bin/env python3
"""Auto-generated by agfs-bench. Do not edit."""
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
import matplotlib.gridspec as gridspec
import matplotlib.lines as mlines
import numpy as np
from matplotlib.ticker import MaxNLocator
from io import StringIO
import csv

DATA = """\
{data_csv}
"""

reader = csv.DictReader(StringIO(DATA.strip()))
rows = list(reader)

ops = [{ops}]
bar_backends = [{bar_backends}]
native_key = '{native_key}'
sources = [{sources}]
source_full = {{'B': 'base', 'S': 'stage', 'C': 'checkpoint'}}
sizes = [100, 10000]
size_labels = {{100: '100 files', 10000: '10K files'}}

{strategy_code}

# Build lookup: (op, size, source, backend) -> lat_us
lookup = {{}}
for r in rows:
    key = (r['op'].strip(), int(r['size'].strip()), r['source'].strip(), r['backend'].strip())
    lookup[key] = float(r['lat_us'].strip())

bar_colors = {{
    'AgFS-NP': '#7bafd4',
    'AgFS':    '#2b6ca3',
    'OvlFS':   '#e8963a',
    'BrFS':    '#c44e52',
}}
native_color = '#555555'

nb = len(bar_backends)
bar_width = 0.8 / nb

def fmt_lat(v):
    if v >= 1_000_000:
        return f'{{v/1_000_000:.0f}}s'
    if v >= 1000:
        return f'{{v/1000:.0f}}ms'
    return f'{{v:.0f}}'

def find_break(vals):
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
    return ((0, sv[best_idx] * 1.3), (sv[best_idx + 1] * 0.85, sv[-1] * 1.15))

def compute_cap(vals):
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
    return reasonable[-1] * 1.35

# ── Figure setup ──
ncols = len(ops)
nrows = 2

if STRATEGY == 'broken':
    from brokenaxes import brokenaxes

fig = plt.figure(figsize=(14, 4.5))
gs = gridspec.GridSpec(nrows, ncols, figure=fig, wspace=0.35, hspace=0.35)

legend_handles = None
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

        # Collect all values (native + bar backends) for scale.
        all_vals = []
        for sk in src_keys:
            for b in [native_key] + bar_backends:
                v = lookup.get((op, size, sk, b), 0)
                if v > 0:
                    all_vals.append(v)
        if not all_vals:
            continue

        # ── Create axis depending on strategy ──
        is_broken = False
        cap = None

        if STRATEGY == 'broken':
            brk = find_break(all_vals)
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
            cap = compute_cap(all_vals)
            ax.set_ylim(0, cap)
        else:  # plain
            ax = fig.add_subplot(gs[row_idx, col_idx])
            ax.set_ylim(0, max(all_vals) * 1.15)

        if not is_broken:
            ax.spines['top'].set_visible(False)
            ax.spines['right'].set_visible(False)

        # ── Draw Native baseline as a single continuous horizontal line ──
        # Use the base variant (or stage for create) as the representative value.
        native_src = 'base' if op != 'create' else 'stage'
        nv = lookup.get((op, size, native_src, native_key), 0)
        if nv > 0:
            ax.axhline(y=nv, color=native_color, linewidth=1.2, linestyle='--',
                       zorder=5, label=native_key if not drew_native_line else None)
            if not drew_native_line:
                drew_native_line = True

        # ── Draw bars for non-native backends ──
        annotations = []
        for bi, b in enumerate(bar_backends):
            vals = [lookup.get((op, size, sk, b), 0) for sk in src_keys]
            offset = (bi - (nb - 1) / 2) * bar_width

            if cap is not None:
                display = [min(v, cap) if v > 0 else 0 for v in vals]
            else:
                display = vals

            bars = ax.bar(x + offset, display, bar_width * 0.9,
                          color=bar_colors[b],
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
                        # Overlay white rectangles with increasing opacity.
                        n_steps = 20
                        step_h = fade_height / n_steps
                        for s in range(n_steps):
                            alpha = (s / (n_steps - 1)) * 0.9  # 0 → 0.9
                            y_bot = fade_bottom + s * step_h
                            ax.bar(xpos, step_h, bw, bottom=y_bot,
                                   color=(1, 1, 1, alpha), edgecolor='none',
                                   zorder=6)
                        # Annotation text in the faded region.
                        annotations.append((xpos, cap * 0.88, fmt_lat(v)))

        for (xp, yp, txt) in annotations:
            ax.annotate(txt, (xp, yp), fontsize=4.5, ha='center', va='center',
                        color='#b00', fontweight='bold', zorder=7)

        # ── Titles (top row only) ──
        if row_idx == 0:
            if is_broken:
                ax.axs[0].set_title(op, fontsize=8, fontweight='bold', pad=3)
            else:
                ax.set_title(op, fontsize=8, fontweight='bold', pad=3)

        # ── Ticks and labels ──
        if is_broken:
            for a in ax.axs:
                a.yaxis.set_major_locator(MaxNLocator(nbins=4, integer=True))
                a.tick_params(axis='y', labelsize=5.5)
                a.tick_params(axis='x', length=0, pad=2)
                a.set_xticks(x)
                a.set_xticklabels([])
            ax.axs[-1].set_xticklabels(src_labels, fontsize=6)
            if col_idx == 0:
                ax.set_ylabel(size_labels[size] + '\nlatency (\u00b5s)',
                              fontsize=7, labelpad=25)
        else:
            ax.set_xticks(x)
            ax.set_xticklabels(src_labels, fontsize=6)
            ax.tick_params(axis='y', labelsize=5.5)
            ax.tick_params(axis='x', length=0, pad=2)
            if col_idx == 0:
                ax.set_ylabel(size_labels[size] + '\nlatency (\u00b5s)', fontsize=7)

# ── Legend ──
# Build custom legend: dashed line for Native, colored patches for bar backends.
legend_items = [
    mlines.Line2D([], [], color=native_color, linestyle='--', linewidth=1.2,
                  label=native_key),
]
for b in bar_backends:
    legend_items.append(
        matplotlib.patches.Patch(facecolor=bar_colors[b], edgecolor='white',
                                 label=b)
    )

fig.legend(handles=legend_items, loc='upper center', ncol=nb + 1, fontsize=7,
           framealpha=0.9, edgecolor='#ccc', borderpad=0.3,
           bbox_to_anchor=(0.5, 1.0))

fig.savefig('{pdf_path}', bbox_inches='tight', dpi=300)
plt.close(fig)
"#,
        data_csv = data_csv,
        ops = ops_py.join(", "),
        bar_backends = bar_backends_py.join(", "),
        native_key = native_name,
        sources = sources_py.join(", "),
        strategy_code = strategy_code,
        pdf_path = pdf_path.display(),
    )
}
