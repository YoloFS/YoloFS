//! Publication table: fio data-operation throughput summary.

use super::Artifact;
use super::util::{backend_display_name, latex_escape, run_pdflatex_cropped};
use crate::BenchResults;
use crate::workload::WorkloadKind;
use crate::workloads;
use anyhow::{Context, Result};
use std::path::Path;

/// Table caption for the float environment.
const CAPTION: &str = "Single-threaded I/O throughput on a 1 GB staged file with 4 KB I/O requests compared with the base Ext4 filesystem.";
/// LaTeX label for cross-referencing.
const LABEL: &str = "tab:op-data";
const TABLEAU_COLOR_DEFS: &str = "\
\\definecolor{TableauBlue}{HTML}{4E79A7}\n\
\\definecolor{TableauOrange}{HTML}{F28E2C}\n\
\\definecolor{TableauGreen}{HTML}{59A14F}\n\
\\definecolor{TableauYellow}{HTML}{EDC949}\n\
\\definecolor{TableauPurple}{HTML}{AF7AA1}\n\
\\definecolor{TableauPink}{HTML}{FF9DA7}\n\
\\definecolor{TableauBrown}{HTML}{9C755F}\n\
\\definecolor{TableauGray}{HTML}{BAB0AB}\n\
\\definecolor{TableauTeal}{HTML}{76B7B2}\n\
\\definecolor{TableauRed}{HTML}{E15759}\n";

pub fn render(results: &BenchResults, paper_dir: &Path) -> Result<Artifact> {
    let tex_path = paper_dir.join("op-data-summary.tex");

    let tex = build_tex(results)?;
    std::fs::write(&tex_path, &tex).with_context(|| format!("writing {}", tex_path.display()))?;

    let pdf_path = match run_pdflatex_cropped(&tex_path, paper_dir) {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("  warning: {e:#}");
            None
        }
    };

    Ok(Artifact {
        group: None,
        title: "Data-op throughput summary (fio)".to_string(),
        preferred: true,
        tex_path: format!("paper/{}", tex_path.file_name().unwrap().to_string_lossy()),
        pdf_path: pdf_path
            .as_ref()
            .map(|p| format!("paper/{}", p.file_name().unwrap().to_string_lossy())),
        tex_abs: tex_path.to_path_buf(),
        plot_pdfs: vec![], // table has no plot PDFs
    })
}

/// Return artifact metadata without rendering (for install-paper).
pub fn artifact_meta(paper_dir: &Path) -> Artifact {
    let tex_path = paper_dir.join("op-data-summary.tex");
    Artifact {
        group: None,
        title: "Data-op throughput summary (fio)".to_string(),
        preferred: true,
        tex_path: format!("paper/{}", tex_path.file_name().unwrap().to_string_lossy()),
        pdf_path: None,
        tex_abs: tex_path,
        plot_pdfs: vec![],
    }
}

// ── Internal ─────────────────────────────────────────────────────────────────

fn build_tex(results: &BenchResults) -> Result<String> {
    let (columns, op_rows) = collect_data(results);

    let mut tex = String::new();

    // Preamble — use acmart sigplan class for publication-matching fonts
    // and metrics. pdfcrop trims to the content bounding box afterward.
    tex.push_str("\\documentclass[sigplan,screen]{acmart}\n");
    tex.push_str("\\settopmatter{printacmref=false,printfolios=false}\n");
    tex.push_str("\\renewcommand\\footnotetextcopyrightpermission[1]{}\n");
    tex.push_str("\\usepackage{multirow}\n");
    tex.push_str("\\usepackage[table]{xcolor}\n");
    tex.push_str("\\usepackage{hhline}\n");
    tex.push_str(TABLEAU_COLOR_DEFS);
    tex.push_str("\\begin{document}\n");
    tex.push_str("\\thispagestyle{empty}\n");

    // The table fragment lives between BEGIN/END markers so it can be
    // extracted and \\input{} into a larger paper .tex file.
    tex.push_str("% --- BEGIN table fragment (includable via \\input) ---\n");
    tex.push_str(TABLEAU_COLOR_DEFS);
    tex.push_str("\\begin{table}[t]\n");
    tex.push_str("\\centering\n");
    tex.push_str("\\small\n");
    tex.push_str("\\setlength{\\tabcolsep}{4pt}\n");

    let col_spec = format!("l@{{\\,}}l@{{\\,}}l|c|{}", "c".repeat(columns.len() - 1));

    tex.push_str(&format!(
        "\\begin{{tabular}}{{{col_spec}}}\n\\noalign{{\\hrule height 0.8pt}}\n"
    ));

    // Header row.
    tex.push_str("\\multicolumn{3}{l|}{Workload}");
    tex.push_str(&format!(" & {}", latex_escape(columns[0].display)));
    for col in &columns[1..] {
        tex.push_str(&format!(" & {}", latex_escape(col.display)));
    }
    tex.push_str(" \\\\\n\\noalign{\\hrule height 0.5pt}\n");

    // Data rows grouped by access pattern, then by operation.
    let structured: Vec<(FioDims, &std::collections::BTreeMap<String, ThroughputVal>)> = op_rows
        .iter()
        .filter_map(|(name, vals)| parse_fio_dims(name).map(|d| (d, vals)))
        .collect();

    let mut i = 0;
    while i < structured.len() {
        let access = structured[i].0.access;
        let mut access_end = i;
        while access_end < structured.len() && structured[access_end].0.access == access {
            access_end += 1;
        }

        let mut j = i;
        while j < access_end {
            let op = structured[j].0.op;
            let mut op_end = j;
            while op_end < access_end && structured[op_end].0.op == op {
                op_end += 1;
            }

            for (k, (dims, vals)) in (j..op_end).zip(&structured[j..op_end]) {
                let Some(native_val) = vals.get("native").copied() else {
                    continue;
                };
                let native_kbps = native_val.mean_kbps;

                // hhline pattern: ~ skips multirow cols, | preserves vrules,
                // - draws a horizontal segment.
                let ncols = columns.len();
                let hhline_op = format!("\\hhline{{~--{}}}", "-".repeat(ncols));
                let hhline_row = format!("\\hhline{{~~-{}}}", "-".repeat(ncols));

                if k == j && j != i {
                    // Between op groups (read/write) within the same access group.
                    tex.push_str(&hhline_op);
                    tex.push('\n');
                } else if k != j {
                    // Between rows within the same op group (cold/warm) — skip op multirow col.
                    tex.push_str(&hhline_row);
                    tex.push('\n');
                }

                // Access column: multirow for first row in this access group.
                if k == i {
                    tex.push_str(&format!(
                        "\\multirow{{{}}}{{*}}{{{}}}",
                        access_end - i,
                        dims.access_label()
                    ));
                }
                tex.push_str(" & ");

                // Op column: multirow for first row in this op group.
                if k == j {
                    tex.push_str(&format!(
                        "\\multirow{{{}}}{{*}}{{{}}}",
                        op_end - j,
                        dims.op_label()
                    ));
                }
                // Omit locality for write (only warm variant exists).
                let locality = if dims.op == OpKind::Write && !dims.cold {
                    ""
                } else {
                    dims.locality_label()
                };
                tex.push_str(&format!(" & {locality}"));

                let noise_pct = native_val
                    .half_range_kbps
                    .filter(|&hr| hr > 0.0 && native_kbps > 0)
                    .map(|hr| hr / native_kbps as f64 * 100.0)
                    .unwrap_or(0.0);

                for (ci, col) in columns.iter().enumerate() {
                    let rendered = if col.key == "native" {
                        format_gbps_with_range(native_val)
                    } else if col.key == "agfs" {
                        rendered_gbps_cell(
                            native_kbps,
                            vals.get("agfs-realistic").map(|v| v.mean_kbps),
                            noise_pct,
                        )
                    } else {
                        rendered_gbps_cell(
                            native_kbps,
                            vals.get(col.key).map(|v| v.mean_kbps),
                            noise_pct,
                        )
                    };
                    // Base uses the real column rule from the tabular spec.
                    if ci == 0 {
                        tex.push_str(&format!(" & {rendered}"));
                    } else {
                        tex.push_str(&format!(" & {rendered}"));
                    }
                }
                tex.push_str(" \\\\\n");
            }
            j = op_end;
        }

        if access_end < structured.len() {
            tex.push_str("\\hline\n");
        }
        i = access_end;
    }

    tex.push_str("\\noalign{\\hrule height 0.8pt}\n\\end{tabular}\n");
    tex.push_str(&format!("\\caption{{{CAPTION}}}\n"));
    tex.push_str(&format!("\\label{{{LABEL}}}\n"));
    tex.push_str("\\end{table}\n");
    tex.push_str("% --- END table fragment ---\n");

    tex.push_str("\\end{document}\n");
    Ok(tex)
}

struct Column {
    /// Internal key (e.g. "agfs-no-perm", "agfs", "overlayfs").
    key: &'static str,
    /// Display name for the header (e.g. "AgFS", "OverlayFS").
    display: &'static str,
}

/// Per-backend throughput data: mean in KB/s and optional half-range in KB/s
/// computed as (max - min) / 2 across iterations.
#[derive(Clone, Copy)]
struct ThroughputVal {
    mean_kbps: u64,
    half_range_kbps: Option<f64>,
}

type OpRow = (String, std::collections::BTreeMap<String, ThroughputVal>);

fn collect_data(results: &BenchResults) -> (Vec<Column>, Vec<OpRow>) {
    let mut op_rows: Vec<OpRow> = Vec::new();

    for wl in &results.workloads {
        let canonical = crate::report::normalize_legacy_workload_name(&wl.workload);
        let kind = workloads::all()
            .into_iter()
            .find(|w| w.name() == canonical)
            .map(|w| w.kind())
            .unwrap_or_else(|| {
                if canonical.starts_with("fio-") {
                    WorkloadKind::Op
                } else {
                    WorkloadKind::Micro
                }
            });
        if kind != WorkloadKind::Op || !canonical.starts_with("fio-") {
            continue;
        }
        if canonical.contains("randrw") {
            continue;
        }

        let mut backend_vals = std::collections::BTreeMap::new();
        for b in &wl.backends {
            if !crate::report::report_backend_visible(&b.backend) {
                continue;
            }
            if let Some(kbps) = b.mean_throughput_kbps {
                // Compute half-range from per-iteration throughput: (max - min) / 2.
                let iter_tps: Vec<f64> = b
                    .iterations
                    .iter()
                    .filter_map(|it| it.op_result.as_ref()?.throughput_kbps.map(|v| v as f64))
                    .collect();
                let half_range_kbps = if iter_tps.len() >= 2 {
                    let mn = iter_tps.iter().copied().fold(f64::INFINITY, f64::min);
                    let mx = iter_tps.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                    Some((mx - mn) / 2.0)
                } else {
                    None
                };
                backend_vals.insert(
                    b.backend.clone(),
                    ThroughputVal {
                        mean_kbps: kbps,
                        half_range_kbps,
                    },
                );
            }
        }
        op_rows.push((canonical, backend_vals));
    }

    op_rows.sort_by_key(|(name, _)| {
        workloads::all()
            .iter()
            .position(|w| w.name() == name)
            .unwrap_or(usize::MAX)
    });

    let mut columns: Vec<Column> = vec![Column {
        key: "native",
        display: "Base (GB/s)",
    }];

    // Single AgFS column using agfs-realistic data only.
    if op_rows
        .iter()
        .any(|(_, v)| v.contains_key("agfs-realistic"))
    {
        columns.push(Column {
            key: "agfs",
            display: backend_display_name("agfs"),
        });
    }

    for name in ["overlayfs", "branchfs"] {
        if op_rows.iter().any(|(_, v)| v.contains_key(name)) {
            columns.push(Column {
                key: match name {
                    "overlayfs" => "overlayfs",
                    "branchfs" => "branchfs",
                    _ => unreachable!(),
                },
                display: backend_display_name(name),
            });
        }
    }

    (columns, op_rows)
}

// ── Formatting helpers ───────────────────────────────────────────────────────

fn format_gbps(kbps: u64) -> String {
    let gbps = kbps as f64 / (1024.0 * 1024.0);
    if gbps >= 0.1 {
        format!("{gbps:.1}")
    } else {
        format!("{gbps:.2}")
    }
}

fn format_gbps_with_range(val: ThroughputVal) -> String {
    let mean = format_gbps(val.mean_kbps);
    match val.half_range_kbps {
        Some(hr) if hr > 0.0 && val.mean_kbps > 0 => {
            let pct = hr / val.mean_kbps as f64 * 100.0;
            if pct >= 0.5 {
                format!("${mean} \\pm {pct:.0}\\%$")
            } else {
                mean
            }
        }
        _ => mean,
    }
}

fn rendered_gbps_cell(native_kbps: u64, kbps: Option<u64>, noise_pct: f64) -> String {
    let Some(kbps) = kbps else {
        return "-".to_string();
    };
    let native_gbps = native_kbps as f64 / (1024.0 * 1024.0);
    let gbps = kbps as f64 / (1024.0 * 1024.0);
    let delta_pct = ((gbps / native_gbps) - 1.0) * 100.0;
    let delta_str = if delta_pct.abs() < 0.5 {
        "0".to_string()
    } else {
        format!("{:+.0}", delta_pct)
    };
    // No coloring if the delta is within the base measurement noise.
    if delta_pct.abs() <= noise_pct {
        format!("${delta_str}\\%$")
    } else if delta_pct < 0.0 {
        let severity = (-delta_pct).clamp(0.0, 100.0);
        let pct = severity / 100.0 * 60.0;
        format!(
            "\\cellcolor{{TableauRed!{:.0}!white}}{{${delta_str}\\%$}}",
            pct
        )
    } else {
        let pct = 25.0_f64.min(delta_pct / 5.0 * 25.0);
        format!(
            "\\cellcolor{{TableauGreen!{:.0}!white}}${delta_str}\\%$",
            pct
        )
    }
}

// ── fio workload dimension parsing ───────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum AccessKind {
    Seq,
    Rand,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OpKind {
    Read,
    Write,
}

#[derive(Clone, Copy)]
struct FioDims {
    access: AccessKind,
    op: OpKind,
    cold: bool,
}

impl FioDims {
    fn access_label(&self) -> &'static str {
        match self.access {
            AccessKind::Seq => "seq",
            AccessKind::Rand => "rand",
        }
    }

    fn op_label(&self) -> &'static str {
        match self.op {
            OpKind::Read => "read",
            OpKind::Write => "write",
        }
    }

    fn locality_label(&self) -> &'static str {
        if self.cold { "cold" } else { "warm" }
    }
}

fn parse_fio_dims(name: &str) -> Option<FioDims> {
    let n = name.strip_prefix("fio-")?;
    if n.contains("randrw") {
        return None;
    }
    let access = if n.starts_with("seq-") {
        AccessKind::Seq
    } else if n.starts_with("rand-") {
        AccessKind::Rand
    } else {
        return None;
    };
    let op = if n.contains("-read") {
        OpKind::Read
    } else if n.contains("-write") {
        OpKind::Write
    } else {
        return None;
    };
    Some(FioDims {
        access,
        op,
        cold: n.ends_with("-cold"),
    })
}
