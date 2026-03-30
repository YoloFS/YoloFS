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
    tex.push_str(TABLEAU_COLOR_DEFS);
    tex.push_str("\\begin{document}\n");
    tex.push_str("\\thispagestyle{empty}\n");

    // The table fragment lives between BEGIN/END markers so it can be
    // extracted and \\input{} into a larger paper .tex file.
    tex.push_str("% --- BEGIN table fragment (includable via \\input) ---\n");
    tex.push_str(TABLEAU_COLOR_DEFS);
    tex.push_str("\\begin{table}[h]\n");
    tex.push_str("\\centering\n");
    tex.push_str("\\small\n");
    tex.push_str("\\setlength{\\tabcolsep}{4pt}\n");

    // Column spec: keep a real vertical separator around the Base column so
    // the rule is continuous through headers and grouped body rows.
    let _ncols = 3 + columns.len();
    let mut col_spec = String::from("l@{\\,}l@{\\,}l");
    for (i, _) in columns.iter().enumerate() {
        if i == 0 {
            col_spec.push_str("|c|");
        } else {
            col_spec.push('c');
        }
    }
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
    let structured: Vec<(FioDims, &std::collections::BTreeMap<String, u64>)> = op_rows
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
                let Some(native_kbps) = vals.get("native").copied() else {
                    continue;
                };

                // Between op groups within the same access group: use \cline so
                // the rule meets the real Base-column vertical separators cleanly.
                if k == j && j != i {
                    let ncols = 3 + columns.len();
                    tex.push_str(&format!("\\cline{{2-{ncols}}}\n"));
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

                for (ci, col) in columns.iter().enumerate() {
                    let rendered = if col.key == "native" {
                        format_gbps(native_kbps)
                    } else if col.key == "agfs" {
                        let kbps = vals
                            .get("agfs-no-perm")
                            .copied()
                            .or_else(|| vals.get("agfs-realistic").copied());
                        rendered_gbps_cell(native_kbps, kbps)
                    } else {
                        rendered_gbps_cell(native_kbps, vals.get(col.key).copied())
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
            tex.push_str("\\noalign{\\hrule height 0.5pt}\n");
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

type OpRow = (String, std::collections::BTreeMap<String, u64>);

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
                backend_vals.insert(b.backend.clone(), kbps);
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

    // Decide whether to merge the two agfs variants into one column.
    let merge_agfs = op_rows.iter().all(|(_, vals)| {
        let Some(native) = vals.get("native") else {
            return true;
        };
        let a = rendered_gbps_cell(*native, vals.get("agfs-no-perm").copied());
        let b = rendered_gbps_cell(*native, vals.get("agfs-realistic").copied());
        a == b
    });

    let mut columns: Vec<Column> = vec![Column {
        key: "native",
        display: "Base (GB/s)",
    }];

    if merge_agfs {
        if op_rows
            .iter()
            .any(|(_, v)| v.contains_key("agfs-no-perm") || v.contains_key("agfs-realistic"))
        {
            columns.push(Column {
                key: "agfs",
                display: backend_display_name("agfs"),
            });
        }
    } else {
        if op_rows.iter().any(|(_, v)| v.contains_key("agfs-no-perm")) {
            columns.push(Column {
                key: "agfs-no-perm",
                display: backend_display_name("agfs-no-perm"),
            });
        }
        if op_rows
            .iter()
            .any(|(_, v)| v.contains_key("agfs-realistic"))
        {
            columns.push(Column {
                key: "agfs-realistic",
                display: backend_display_name("agfs-realistic"),
            });
        }
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

fn rendered_gbps_cell(native_kbps: u64, kbps: Option<u64>) -> String {
    let Some(kbps) = kbps else {
        return "-".to_string();
    };
    let native_gbps = native_kbps as f64 / (1024.0 * 1024.0);
    let gbps = kbps as f64 / (1024.0 * 1024.0);
    let delta_pct = ((gbps / native_gbps) - 1.0) * 100.0;
    if delta_pct.abs() < 5.0 {
        "\\cellcolor{TableauGreen!25!white}<5\\%".to_string()
    } else if delta_pct < 0.0 {
        let severity = (-delta_pct).clamp(5.0, 100.0);
        let pct = 18.0 + (severity - 5.0) / 95.0 * 42.0;
        format!(
            "\\cellcolor{{TableauRed!{:.0}!white}}{{{:+.0}\\%}}",
            pct, delta_pct
        )
    } else {
        format!("{:+.0}\\%", delta_pct)
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
