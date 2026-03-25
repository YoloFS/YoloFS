// HTML report generation using plotly.

use crate::backends;
use crate::workload::WorkloadKind;
use crate::workloads;
use crate::{BenchResults, RepoState, WorkloadResult};
use anyhow::{Context, Result};
use plotly::common::{
    ErrorData, ErrorType, Font, HoverInfo, Mode, Pattern, PatternFillMode, PatternShape,
    TextPosition, Title,
};
use plotly::layout::{Annotation, Axis, BarMode, Shape, ShapeLayer, ShapeLine, ShapeType};
use plotly::{Bar, Configuration, Layout, Plot, Scatter};
use std::path::Path;

pub fn render(results: &BenchResults, out_dir: &Path) -> Result<()> {
    let current_repo_state = crate::read_repo_state().ok();
    let mut rendered_groups: std::collections::HashSet<String> = std::collections::HashSet::new();
    for wl in &results.workloads {
        if let Some(group) = source_group_name(&wl.workload) {
            if rendered_groups.insert(group.to_string()) {
                render_grouped_op_workloads(group, results, out_dir, current_repo_state.as_ref())?;
            }
        } else if is_stale_group_name(&wl.workload, results) {
            // Skip stale entries like "meta-unlink" when actual source
            // variants (meta-unlink-base, etc.) exist in results.
            continue;
        } else {
            render_workload(wl, results, out_dir, current_repo_state.as_ref())?;
        }
    }
    crate::paper::render(results, out_dir)?;
    render_commit_scaling_report(out_dir)?;
    render_index(results, out_dir, current_repo_state.as_ref())?;
    Ok(())
}

pub fn render_commit_scaling_report(out_dir: &Path) -> Result<()> {
    let json_path = out_dir.join("commit-scaling.json");
    if !json_path.exists() {
        return Ok(());
    }

    #[derive(serde::Deserialize)]
    struct CommitScalingResult {
        #[serde(default)]
        backend: String,
        op: String,
        points: Vec<CommitScalingPoint>,
    }
    #[derive(serde::Deserialize)]
    struct CommitScalingPoint {
        count: usize,
        staging_ms: u64,
        commit_ms: u64,
    }

    let data: Vec<CommitScalingResult> =
        serde_json::from_str(&std::fs::read_to_string(&json_path)?)?;

    let mut html = String::new();
    html.push_str("<!DOCTYPE html><html><head><meta charset=\"utf-8\">\n");
    html.push_str("<title>Commit Scaling</title>\n");
    html.push_str("<script src=\"https://cdn.plot.ly/plotly-2.35.2.min.js\"></script>\n");
    html.push_str("</head><body>\n");
    html.push_str("<h2>Commit time vs file count</h2>\n");
    html.push_str("<div id=\"commit-plot\" style=\"width:900px;height:500px\"></div>\n");
    html.push_str("<h2>Staging time vs file count</h2>\n");
    html.push_str("<div id=\"staging-plot\" style=\"width:900px;height:500px\"></div>\n");
    html.push_str("<script>\n");

    for (div_id, field) in [("commit-plot", "commit"), ("staging-plot", "staging")] {
        html.push_str(&format!("Plotly.newPlot('{div_id}', [\n"));
        for res in &data {
            let xs: Vec<String> = res.points.iter().map(|p| p.count.to_string()).collect();
            let ys: Vec<String> = res.points.iter().map(|p| {
                if field == "commit" { p.commit_ms } else { p.staging_ms }.to_string()
            }).collect();
            let trace_name = if res.backend.is_empty() {
                res.op.clone()
            } else {
                format!("{} ({})", res.op, res.backend)
            };
            html.push_str(&format!(
                "  {{x: [{}], y: [{}], mode: 'lines+markers', name: '{}'}},\n",
                xs.join(","), ys.join(","), trace_name
            ));
        }
        let ylabel = if field == "commit" { "Commit time (ms)" } else { "Staging time (ms)" };
        html.push_str(&format!(
            "], {{xaxis: {{title: 'File count'}}, yaxis: {{title: '{ylabel}'}}}});\n"
        ));
    }

    html.push_str("</script></body></html>\n");

    let html_path = out_dir.join("report-commit-scaling.html");
    std::fs::write(&html_path, &html)?;
    eprintln!("Report written to {}", html_path.display());
    Ok(())
}

pub fn render_paper_only(results: &BenchResults, out_dir: &Path) -> Result<()> {
    crate::paper::render(results, out_dir)
}

pub fn render_one(results: &BenchResults, workload_name: &str, out_dir: &Path) -> Result<()> {
    let current_repo_state = crate::read_repo_state().ok();
    if let Some(group) = source_group_name(workload_name) {
        render_grouped_op_workloads(group, results, out_dir, current_repo_state.as_ref())?;
    } else if let Some(wl) = results
        .workloads
        .iter()
        .find(|w| w.workload == workload_name)
    {
        render_workload(wl, results, out_dir, current_repo_state.as_ref())?;
    }
    render_index(results, out_dir, current_repo_state.as_ref())?;
    Ok(())
}

fn render_workload(
    wl: &WorkloadResult,
    _results: &BenchResults,
    out_dir: &Path,
    current_repo_state: Option<&RepoState>,
) -> Result<()> {
    let has_checkpoint_series = wl.backends.iter().any(|b| b.checkpoint_series.is_some());
    if has_checkpoint_series {
        return render_checkpoint_scalability_workload(wl, out_dir, current_repo_state);
    }
    let is_op = wl.backends.iter().any(|b| b.mean_iops.is_some());
    if is_op {
        render_op_workload(wl, out_dir, current_repo_state)
    } else {
        render_session_workload(wl, out_dir, current_repo_state)
    }
}

fn render_checkpoint_scalability_workload(
    wl: &WorkloadResult,
    out_dir: &Path,
    current_repo_state: Option<&RepoState>,
) -> Result<()> {
    type OpLatFn = fn(&crate::workload::CheckpointLatencyPoint) -> f64;
    let order = backends::display_order();
    let mut sorted = wl.backends.clone();
    sorted.retain(|b| report_backend_visible(&b.backend));
    sorted.sort_by_key(|b| {
        order
            .iter()
            .position(|&name| name == b.backend)
            .unwrap_or(usize::MAX)
    });

    let op_defs: [(&str, OpLatFn); 6] = [
        ("stat", |p| p.stat_avg_lat_us),
        ("readdir", |p| p.readdir_avg_lat_us),
        ("unlink", |p| p.unlink_avg_lat_us),
        ("read", |p| p.read_avg_lat_us),
        ("create", |p| p.create_avg_lat_us),
        ("overwrite", |p| p.overwrite_avg_lat_us),
    ];

    for (op_name, get_lat) in &op_defs {
        let mut plot = Plot::new();
        for b in &sorted {
            let Some(series) = &b.checkpoint_series else {
                continue;
            };
            let xs: Vec<u32> = series.points.iter().map(|p| p.checkpoint).collect();
            let ys: Vec<f64> = series.points.iter().map(get_lat).collect();
            plot.add_trace(
                Scatter::new(xs.clone(), ys)
                    .mode(Mode::LinesMarkers)
                    .name(b.backend.clone()),
            );
        }
        let layout = Layout::new()
            .title(Title::with_text(format!("{} - {}", wl.workload, op_name)))
            .x_axis(Axis::new().title(Title::with_text("checkpoint number")))
            .y_axis(Axis::new().title(Title::with_text("avg latency per op (us)")))
            .show_legend(true);
        plot.set_layout(layout);
        plot.set_configuration(Configuration::new().responsive(true).fill_frame(true));

        let op_html_path = out_dir.join(format!("report-{}-{}.html", wl.workload, op_name));
        std::fs::write(&op_html_path, plot.to_html())
            .with_context(|| format!("writing {}", op_html_path.display()))?;
    }

    let html_path = out_dir.join(format!("report-{}.html", wl.workload));
    let status = workload_staleness(wl, current_repo_state);
    let mut full_html = String::new();
    full_html.push_str("<!DOCTYPE html><html><head><meta charset=\"utf-8\">");
    full_html.push_str(&format!("<title>{}</title>", escape_html(&wl.workload)));
    full_html.push_str(
        "<style>body{font-family:system-ui,sans-serif;margin:1em;background:#fafafa}h1{font-size:1.15em}p{color:#555}.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(560px,1fr));gap:1em}iframe{width:100%;height:420px;border:1px solid #ddd;border-radius:4px;background:#fff}</style>",
    );
    full_html.push_str("</head><body>");
    full_html.push_str(&format!("<h1>{}</h1>", escape_html(&wl.workload)));
    full_html.push_str(
        "<p>Checkpoint scalability facets: one line plot per operation (lines = backends).</p>",
    );
    full_html.push_str("<div class=\"grid\">");
    for (op_name, _) in &op_defs {
        full_html.push_str(&format!(
            "<iframe src=\"report-{}-{}.html\"></iframe>",
            wl.workload, op_name
        ));
    }
    full_html.push_str("</div></body></html>");
    full_html = inject_workload_status(full_html, &status);
    std::fs::write(&html_path, full_html)
        .with_context(|| format!("writing {}", html_path.display()))?;
    eprintln!("Report written to {}", html_path.display());
    Ok(())
}

fn render_session_workload(
    wl: &WorkloadResult,
    out_dir: &Path,
    current_repo_state: Option<&RepoState>,
) -> Result<()> {
    let mut plot = Plot::new();

    let order = backends::display_order();
    let mut sorted = wl.backends.clone();
    sorted.retain(|b| report_backend_visible(&b.backend));
    sorted.sort_by_key(|b| {
        order
            .iter()
            .position(|&name| name == b.backend)
            .unwrap_or(usize::MAX)
    });

    let native_ms = sorted
        .iter()
        .find(|b| b.backend == "native")
        .map(|b| b.mean_total_ms);
    sorted.retain(|b| b.backend != "native");

    let backend_names: Vec<String> = sorted.iter().map(|b| b.backend.clone()).collect();

    let init_vals: Vec<f64> = sorted
        .iter()
        .map(|b| b.mean_init_ms.unwrap_or(0.0))
        .collect();

    let staging_vals: Vec<f64> = sorted
        .iter()
        .map(|b| b.mean_staging_ms.unwrap_or(b.mean_total_ms))
        .collect();

    let commit_vals: Vec<f64> = sorted
        .iter()
        .map(|b| b.mean_commit_ms.unwrap_or(0.0))
        .collect();

    let stddev_vals: Vec<f64> = sorted.iter().map(|b| b.stddev_total_ms).collect();

    plot.add_trace(Bar::new(backend_names.clone(), init_vals).name("init"));
    plot.add_trace(Bar::new(backend_names.clone(), staging_vals).name("staging"));
    plot.add_trace(
        Bar::new(backend_names.clone(), commit_vals)
            .name("commit")
            .error_y(
                ErrorData::new(ErrorType::Data)
                    .array(stddev_vals)
                    .visible(true),
            ),
    );

    let mut layout = Layout::new()
        .bar_mode(BarMode::Stack)
        .title(Title::with_text(wl.workload.clone()))
        .x_axis(Axis::new().title(Title::with_text("backend")))
        .y_axis(Axis::new().title(Title::with_text("time (ms)")));
    if let Some(native) = native_ms {
        add_native_baseline_hover_trace(&mut plot, &backend_names, native, "native total");
        layout = add_native_baseline_annotation(layout, native);
    }

    plot.set_layout(layout);
    plot.set_configuration(Configuration::new().responsive(true).fill_frame(true));

    let html_path = out_dir.join(format!("report-{}.html", wl.workload));
    let status = workload_staleness(wl, current_repo_state);
    let mut full_html = plot.to_html();
    full_html = inject_workload_status(full_html, &status);
    std::fs::write(&html_path, full_html)
        .with_context(|| format!("writing {}", html_path.display()))?;
    eprintln!("Report written to {}", html_path.display());
    Ok(())
}

fn render_op_workload(
    wl: &WorkloadResult,
    out_dir: &Path,
    current_repo_state: Option<&RepoState>,
) -> Result<()> {
    if wl
        .backends
        .iter()
        .any(|b| b.mean_read_avg_lat_us.is_some() && b.mean_write_avg_lat_us.is_some())
    {
        return render_op_mixed_workload(wl, out_dir, current_repo_state);
    }

    let mut plot = Plot::new();

    let order = backends::display_order();
    let mut sorted = wl.backends.clone();
    sorted.retain(|b| report_backend_visible(&b.backend));
    sorted.sort_by_key(|b| {
        order
            .iter()
            .position(|&name| name == b.backend)
            .unwrap_or(usize::MAX)
    });

    let native_iops = sorted
        .iter()
        .find(|b| b.backend == "native")
        .and_then(|b| b.mean_iops);
    sorted.retain(|b| b.backend != "native");

    let backend_names: Vec<String> = sorted.iter().map(|b| b.backend.clone()).collect();
    let avg_lat_vals: Vec<f64> = sorted
        .iter()
        .map(|b| avg_latency_us(b.mean_iops.unwrap_or(0.0)))
        .collect();
    let stddev_vals: Vec<f64> = sorted
        .iter()
        .map(|b| avg_latency_stddev_us(b.mean_iops.unwrap_or(0.0), b.stddev_iops.unwrap_or(0.0)))
        .collect();
    let bar_colors: Vec<String> = sorted
        .iter()
        .map(|b| backend_color(&b.backend).to_string())
        .collect();

    let native_avg_lat = native_iops.map(avg_latency_us);
    add_capped_op_trace(
        &mut plot,
        &backend_names,
        &avg_lat_vals,
        &stddev_vals,
        &bar_colors,
        native_avg_lat,
        OpBarStyle {
            opacity: None,
            line: None,
            x_axis: None,
            y_axis: None,
            offset_group: None,
            alignment_group: None,
        },
    );

    let (mut layout, mut shapes, mut annotations) = op_layout(wl, false);
    if let Some(native) = native_iops {
        add_baseline_to_plot(
            &mut plot,
            &mut shapes,
            &mut annotations,
            &backend_names,
            BaselineSpec {
                value: avg_latency_us(native),
                hover_label: "native avg latency (µs)",
                visible_label: "native baseline",
                secondary_shift: false,
            },
        );
    }
    if !shapes.is_empty() {
        layout = layout.shapes(shapes).annotations(annotations);
    }
    plot.set_layout(layout);
    plot.set_configuration(Configuration::new().responsive(true).fill_frame(true));

    // Write the plotly chart, then append a latency table.
    let html_path = out_dir.join(format!("report-{}.html", wl.workload));
    let mut full_html = plot.to_html();
    let status = workload_staleness(wl, current_repo_state);

    // Inject latency table before </body>.
    let mut table = String::from(
        "<table style=\"margin:1em auto;border-collapse:collapse;font-family:system-ui;font-size:0.85em\">\n\
         <tr style=\"border-bottom:1px solid #ccc\">\
         <th style=\"padding:0.3em 1em;text-align:left\">backend</th>\
         <th style=\"padding:0.3em 1em\">avg latency (µs)</th>\
         <th style=\"padding:0.3em 1em\">p50 (µs)</th>\
         <th style=\"padding:0.3em 1em\">p99 (µs)</th>",
    );
    if sorted.iter().any(|b| b.mean_throughput_kbps.is_some()) {
        table.push_str("<th style=\"padding:0.3em 1em\">throughput (MB/s)</th>");
    }
    table.push_str("</tr>\n");

    for b in &sorted {
        table.push_str(&format!(
            "<tr><td style=\"padding:0.3em 1em\">{}</td>\
             <td style=\"padding:0.3em 1em;text-align:right\">{:.0}</td>\
             <td style=\"padding:0.3em 1em;text-align:right\">{:.1}</td>\
             <td style=\"padding:0.3em 1em;text-align:right\">{:.1}</td>",
            b.backend,
            avg_latency_us(b.mean_iops.unwrap_or(0.0)),
            b.mean_lat_us_p50.unwrap_or(0.0),
            b.mean_lat_us_p99.unwrap_or(0.0),
        ));
        if sorted.iter().any(|b| b.mean_throughput_kbps.is_some()) {
            let mbps = b.mean_throughput_kbps.unwrap_or(0) as f64 / 1024.0;
            table.push_str(&format!(
                "<td style=\"padding:0.3em 1em;text-align:right\">{mbps:.1}</td>"
            ));
        }
        table.push_str("</tr>\n");
    }
    table.push_str("</table>\n");

    full_html = inject_workload_status(full_html, &status);
    full_html = full_html.replace("</body>", &format!("{table}</body>"));

    std::fs::write(&html_path, full_html)
        .with_context(|| format!("writing {}", html_path.display()))?;
    eprintln!("Report written to {}", html_path.display());
    Ok(())
}

fn render_op_mixed_workload(
    wl: &WorkloadResult,
    out_dir: &Path,
    current_repo_state: Option<&RepoState>,
) -> Result<()> {
    let mut plot = Plot::new();

    let order = backends::display_order();
    let mut sorted = wl.backends.clone();
    sorted.retain(|b| report_backend_visible(&b.backend));
    sorted.sort_by_key(|b| {
        order
            .iter()
            .position(|&name| name == b.backend)
            .unwrap_or(usize::MAX)
    });

    let native = sorted.iter().find(|b| b.backend == "native").cloned();
    sorted.retain(|b| b.backend != "native");

    let backend_names: Vec<String> = sorted.iter().map(|b| b.backend.clone()).collect();
    let colors: Vec<String> = sorted
        .iter()
        .map(|b| backend_color(&b.backend).to_string())
        .collect();
    let read_vals: Vec<f64> = sorted
        .iter()
        .map(|b| b.mean_read_avg_lat_us.unwrap_or(0.0))
        .collect();
    let read_stddevs: Vec<f64> = sorted
        .iter()
        .map(|b| b.stddev_read_avg_lat_us.unwrap_or(0.0))
        .collect();
    let write_vals: Vec<f64> = sorted
        .iter()
        .map(|b| b.mean_write_avg_lat_us.unwrap_or(0.0))
        .collect();
    let write_stddevs: Vec<f64> = sorted
        .iter()
        .map(|b| b.stddev_write_avg_lat_us.unwrap_or(0.0))
        .collect();
    let native_read = native.as_ref().and_then(|b| b.mean_read_avg_lat_us);
    add_capped_op_trace(
        &mut plot,
        &backend_names,
        &read_vals,
        &read_stddevs,
        &colors,
        native_read,
        OpBarStyle {
            opacity: None,
            line: None,
            x_axis: None,
            y_axis: None,
            offset_group: Some("read"),
            alignment_group: Some("rw"),
        },
    );

    let native_write = native.as_ref().and_then(|b| b.mean_write_avg_lat_us);
    add_capped_op_trace(
        &mut plot,
        &backend_names,
        &write_vals,
        &write_stddevs,
        &colors,
        native_write,
        OpBarStyle {
            opacity: Some(0.35),
            line: Some(plotly::common::Line::new().color("#222").width(1.5)),
            x_axis: None,
            y_axis: None,
            offset_group: Some("write"),
            alignment_group: Some("rw"),
        },
    );

    let (mut layout, mut shapes, mut annotations) = op_layout(wl, true);
    if let Some(native) = native {
        if let Some(read_lat) = native.mean_read_avg_lat_us {
            add_baseline_to_plot(
                &mut plot,
                &mut shapes,
                &mut annotations,
                &backend_names,
                BaselineSpec {
                    value: read_lat,
                    hover_label: "native read avg latency (µs)",
                    visible_label: "native read",
                    secondary_shift: true,
                },
            );
        }
        if let Some(write_lat) = native.mean_write_avg_lat_us {
            add_baseline_to_plot(
                &mut plot,
                &mut shapes,
                &mut annotations,
                &backend_names,
                BaselineSpec {
                    value: write_lat,
                    hover_label: "native write avg latency (µs)",
                    visible_label: "native write",
                    secondary_shift: true,
                },
            );
        }
    }
    if !shapes.is_empty() {
        layout = layout.shapes(shapes).annotations(annotations);
    }
    plot.set_layout(layout);
    plot.set_configuration(Configuration::new().responsive(true).fill_frame(true));

    let html_path = out_dir.join(format!("report-{}.html", wl.workload));
    let mut full_html = plot.to_html();
    let status = workload_staleness(wl, current_repo_state);

    let mut table = String::from(
        "<div style=\"margin:0.6em auto 0;max-width:960px;color:#666;font:0.82em system-ui,sans-serif\">solid = read avg latency, translucent outlined = write avg latency</div>\
         <table style=\"margin:1em auto;border-collapse:collapse;font-family:system-ui;font-size:0.85em\">\n\
         <tr style=\"border-bottom:1px solid #ccc\">\
         <th style=\"padding:0.3em 1em;text-align:left\">backend</th>\
         <th style=\"padding:0.3em 1em\">read avg (µs)</th>\
         <th style=\"padding:0.3em 1em\">read p50 (µs)</th>\
         <th style=\"padding:0.3em 1em\">read p99 (µs)</th>\
         <th style=\"padding:0.3em 1em\">write avg (µs)</th>\
         <th style=\"padding:0.3em 1em\">write p50 (µs)</th>\
         <th style=\"padding:0.3em 1em\">write p99 (µs)</th>\
         <th style=\"padding:0.3em 1em\">throughput (MB/s)</th></tr>\n",
    );
    for b in &sorted {
        let mbps = b.mean_throughput_kbps.unwrap_or(0) as f64 / 1024.0;
        table.push_str(&format!(
            "<tr><td style=\"padding:0.3em 1em\">{}</td>\
             <td style=\"padding:0.3em 1em;text-align:right\">{:.1}</td>\
             <td style=\"padding:0.3em 1em;text-align:right\">{:.1}</td>\
             <td style=\"padding:0.3em 1em;text-align:right\">{:.1}</td>\
             <td style=\"padding:0.3em 1em;text-align:right\">{:.1}</td>\
             <td style=\"padding:0.3em 1em;text-align:right\">{:.1}</td>\
             <td style=\"padding:0.3em 1em;text-align:right\">{:.1}</td>\
             <td style=\"padding:0.3em 1em;text-align:right\">{:.1}</td></tr>\n",
            b.backend,
            b.mean_read_avg_lat_us.unwrap_or(0.0),
            b.mean_read_lat_us_p50.unwrap_or(0.0),
            b.mean_read_lat_us_p99.unwrap_or(0.0),
            b.mean_write_avg_lat_us.unwrap_or(0.0),
            b.mean_write_lat_us_p50.unwrap_or(0.0),
            b.mean_write_lat_us_p99.unwrap_or(0.0),
            mbps,
        ));
    }
    table.push_str("</table>\n");

    full_html = inject_workload_status(full_html, &status);
    full_html = full_html.replace("</body>", &format!("{table}</body>"));

    std::fs::write(&html_path, full_html)
        .with_context(|| format!("writing {}", html_path.display()))?;
    eprintln!("Report written to {}", html_path.display());
    Ok(())
}

fn avg_latency_us(iops: f64) -> f64 {
    if iops <= 0.0 { 0.0 } else { 1_000_000.0 / iops }
}

fn avg_latency_stddev_us(mean_iops: f64, stddev_iops: f64) -> f64 {
    if mean_iops <= 0.0 || stddev_iops <= 0.0 {
        0.0
    } else {
        1_000_000.0 * stddev_iops / (mean_iops * mean_iops)
    }
}

fn backend_color(backend: &str) -> &'static str {
    match backend {
        "agfs-no-perm" => "#2E86AB",
        "agfs-realistic" => "#4F772D",
        "overlayfs" => "#C1666B",
        "branchfs" => "#D17B0F",
        _ => "#4C6EF5",
    }
}

#[derive(Clone, Copy)]
enum FreshnessKind {
    Fresh,
    Stale,
    Unknown,
}

struct WorkloadFreshness {
    kind: FreshnessKind,
    summary: String,
    reasons: Vec<String>,
    commit_label: Option<String>,
}

fn workload_staleness(wl: &WorkloadResult, current: Option<&RepoState>) -> WorkloadFreshness {
    let mut reasons = Vec::new();
    let commit_label = wl
        .backends
        .iter()
        .filter_map(|backend| {
            backend
                .repo_state
                .as_ref()
                .map(|state| short_commit(&state.commit))
        })
        .next()
        .map(str::to_string);

    for backend in &wl.backends {
        if !report_backend_visible(&backend.backend) {
            continue;
        }
        match (&backend.repo_state, current) {
            (Some(recorded), Some(current)) => {
                let backend_reasons = repo_state_drift(recorded, current);
                reasons.extend(
                    backend_reasons
                        .into_iter()
                        .map(|reason| format!("{}: {}", backend.backend, reason)),
                );
            }
            (None, _) => reasons.push(format!(
                "{}: recorded before repo-state tracking was added",
                backend.backend
            )),
            (Some(_), None) => reasons.push(format!(
                "{}: current repo state could not be probed",
                backend.backend
            )),
        }
    }

    let kind = if reasons.is_empty() {
        FreshnessKind::Fresh
    } else if current.is_none() {
        FreshnessKind::Unknown
    } else {
        FreshnessKind::Stale
    };

    let summary = match kind {
        FreshnessKind::Fresh => "fresh".to_string(),
        FreshnessKind::Stale => format!("stale ({})", reasons.len()),
        FreshnessKind::Unknown => "unknown".to_string(),
    };

    WorkloadFreshness {
        kind,
        summary,
        reasons,
        commit_label,
    }
}

fn repo_state_drift(recorded: &RepoState, current: &RepoState) -> Vec<String> {
    let mut reasons = Vec::new();
    if recorded.commit != current.commit {
        match crate::repo_paths_changed_between(&recorded.commit, &current.commit) {
            Ok(true) => reasons.push(format!(
                "user/ or kmod/ changed between {} and {}",
                short_commit(&recorded.commit),
                short_commit(&current.commit)
            )),
            Ok(false) => {}
            Err(_) => reasons.push(format!(
                "could not compare user/ and kmod/ between {} and {}",
                short_commit(&recorded.commit),
                short_commit(&current.commit)
            )),
        }
    }
    if recorded.user_dirty != current.user_dirty {
        reasons.push(format!(
            "user/ was {} and is now {}",
            dirty_label(recorded.user_dirty),
            dirty_label(current.user_dirty)
        ));
    }
    if recorded.kmod_dirty != current.kmod_dirty {
        reasons.push(format!(
            "kmod/ was {} and is now {}",
            dirty_label(recorded.kmod_dirty),
            dirty_label(current.kmod_dirty)
        ));
    }
    reasons
}

pub fn report_backend_visible(name: &str) -> bool {
    // Hide stale pre-rename results from report plots/tables.
    name != "agfs-allow-all"
}

fn dirty_label(dirty: bool) -> &'static str {
    if dirty { "dirty" } else { "clean" }
}

fn short_commit(commit: &str) -> &str {
    commit.get(..6).unwrap_or(commit)
}

fn inject_workload_status(mut full_html: String, status: &WorkloadFreshness) -> String {
    let color = match status.kind {
        FreshnessKind::Fresh => "#1f7a1f",
        FreshnessKind::Stale => "#b54a00",
        FreshnessKind::Unknown => "#666",
    };
    let mut block = format!(
        "<div style=\"margin:1em auto;max-width:960px;padding:0.75em 1em;border:1px solid #ddd;border-radius:6px;background:#fff;font-family:system-ui,sans-serif\">\
         <div style=\"font-size:0.95em\"><strong style=\"color:{color}\">status: {}</strong></div>",
        escape_html(&status.summary)
    );
    if !status.reasons.is_empty() {
        block
            .push_str("<ul style=\"margin:0.5em 0 0 1.2em;padding:0;color:#555;font-size:0.9em\">");
        for reason in &status.reasons {
            block.push_str(&format!("<li>{}</li>", escape_html(reason)));
        }
        block.push_str("</ul>");
    }
    block.push_str("</div>");
    full_html = full_html.replace("</body>", &format!("{block}</body>"));
    full_html
}

pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn preserve_multiline_html(s: &str) -> String {
    let escaped = escape_html(s);
    if escaped.contains("```") {
        render_fenced_blocks(&escaped)
    } else {
        escaped.replace('\n', "<br>")
    }
}

fn render_fenced_blocks(s: &str) -> String {
    let mut out = String::new();
    let mut in_code = false;
    for part in s.split("```") {
        if in_code {
            let code = strip_fence_language(part);
            out.push_str("<pre style=\"overflow:auto;background:#f6f6f6;border:1px solid #e1e1e1;padding:0.7em;border-radius:6px\"><code>");
            out.push_str(code);
            out.push_str("</code></pre>");
        } else {
            out.push_str(&part.replace('\n', "<br>"));
        }
        in_code = !in_code;
    }
    out
}

fn strip_fence_language(s: &str) -> &str {
    let Some((first_line, rest)) = s.split_once('\n') else {
        return s;
    };
    if !first_line.is_empty()
        && first_line
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        rest
    } else {
        s
    }
}

fn add_native_baseline_hover_trace(
    plot: &mut Plot,
    backend_names: &[String],
    native: f64,
    label: &str,
) {
    if backend_names.is_empty() {
        return;
    }

    plot.add_trace(
        Scatter::new(backend_names.to_vec(), vec![native; backend_names.len()])
            .mode(Mode::Lines)
            .name("native baseline")
            .show_legend(false)
            .opacity(0.0)
            .hover_info(HoverInfo::Text)
            .hover_text_array(vec![format!("{label}: {native:.1}"); backend_names.len()]),
    );
}

fn add_native_baseline_annotation(layout: Layout, native: f64) -> Layout {
    layout
        .shapes(vec![baseline_shape(native, "y", "paper")])
        .annotations(vec![baseline_annotation(
            native,
            "native baseline",
            "y",
            "paper",
            -10.0,
        )])
}

fn op_bar_trace(
    backend_names: Vec<String>,
    values: Vec<f64>,
    stddevs: Vec<f64>,
    colors: Vec<String>,
    style: OpBarStyle,
) -> Box<Bar<String, f64>> {
    let mut marker = plotly::common::Marker::new().color_array(colors);
    if let Some(line) = style.line {
        marker = marker.line(line);
    }
    let mut trace = Bar::new(backend_names, values)
        .show_legend(false)
        .marker(marker)
        .error_y(ErrorData::new(ErrorType::Data).array(stddevs).visible(true));
    if let Some(opacity) = style.opacity {
        trace = trace.opacity(opacity);
    }
    if let Some(x_axis) = style.x_axis {
        trace = trace.x_axis(x_axis);
    }
    if let Some(y_axis) = style.y_axis {
        trace = trace.y_axis(y_axis);
    }
    if let Some(offset_group) = style.offset_group {
        trace = trace.offset_group(offset_group);
    }
    if let Some(alignment_group) = style.alignment_group {
        trace = trace.alignment_group(alignment_group);
    }
    trace
}

#[derive(Clone)]
struct OpBarStyle {
    opacity: Option<f64>,
    line: Option<plotly::common::Line>,
    x_axis: Option<&'static str>,
    y_axis: Option<&'static str>,
    offset_group: Option<&'static str>,
    alignment_group: Option<&'static str>,
}

struct OutlierCap {
    display_values: Vec<f64>,
    display_stddevs: Vec<f64>,
    labels: Vec<String>,
    main_indices: Vec<usize>,
    outlier_indices: Vec<usize>,
}

fn cap_relative_to_native(values: &[f64], stddevs: &[f64], native: Option<f64>) -> OutlierCap {
    let mut display_values = values.to_vec();
    let mut display_stddevs = stddevs.to_vec();
    let mut labels = vec![String::new(); values.len()];
    let mut outlier_indices = Vec::new();
    let mut main_indices = Vec::new();

    let Some(native) = native.filter(|v| *v > 0.0) else {
        main_indices.extend(0..values.len());
        return OutlierCap {
            display_values,
            display_stddevs,
            labels,
            main_indices,
            outlier_indices,
        };
    };

    let visible_cap = values
        .iter()
        .copied()
        .filter(|v| *v > 0.0 && *v < native * 10.0)
        .fold(native, f64::max)
        * 1.15;

    for (idx, value) in values.iter().copied().enumerate() {
        if value >= native * 10.0 {
            display_values[idx] = visible_cap;
            display_stddevs[idx] = 0.0;
            labels[idx] = format!("{:.0}x native", value / native);
            outlier_indices.push(idx);
        } else {
            main_indices.push(idx);
        }
    }

    OutlierCap {
        display_values,
        display_stddevs,
        labels,
        main_indices,
        outlier_indices,
    }
}

fn op_layout(wl: &WorkloadResult, grouped: bool) -> (Layout, Vec<Shape>, Vec<Annotation>) {
    let mut layout = Layout::new()
        .title(Title::with_text(wl.workload.clone()))
        .x_axis(Axis::new().title(Title::with_text("backend")))
        .y_axis(Axis::new().title(Title::with_text("avg latency (µs)")))
        .show_legend(false);
    let annotations = Vec::new();
    if grouped {
        layout = layout.bar_mode(BarMode::Group);
    }
    (layout, Vec::new(), annotations)
}

fn add_baseline_to_plot(
    plot: &mut Plot,
    shapes: &mut Vec<Shape>,
    annotations: &mut Vec<Annotation>,
    backend_names: &[String],
    baseline: BaselineSpec<'_>,
) {
    add_native_baseline_hover_trace(plot, backend_names, baseline.value, baseline.hover_label);
    let y_shift = if baseline.secondary_shift {
        10.0
    } else {
        -10.0
    };
    shapes.push(baseline_shape(baseline.value, "y", "paper"));
    annotations.push(baseline_annotation(
        baseline.value,
        baseline.visible_label,
        "y",
        "paper",
        y_shift,
    ));
}

struct BaselineSpec<'a> {
    value: f64,
    hover_label: &'a str,
    visible_label: &'a str,
    secondary_shift: bool,
}

fn baseline_shape(value: f64, y_ref: &str, x_ref: &str) -> Shape {
    Shape::new()
        .shape_type(ShapeType::Line)
        .layer(ShapeLayer::Above)
        .x_ref(x_ref)
        .x0(0.0)
        .x1(1.0)
        .y_ref(y_ref)
        .y0(value)
        .y1(value)
        .line(ShapeLine::new().dash(plotly::common::DashType::Dot))
}

fn baseline_annotation(
    value: f64,
    label: &str,
    y_ref: &str,
    x_ref: &str,
    y_shift: f64,
) -> Annotation {
    Annotation::new()
        .x_ref(x_ref)
        .x(1.0)
        .y_ref(y_ref)
        .y(value)
        .text(label)
        .show_arrow(false)
        .y_shift(y_shift)
}

fn add_capped_op_trace(
    plot: &mut Plot,
    names: &[String],
    values: &[f64],
    stddevs: &[f64],
    colors: &[String],
    native: Option<f64>,
    style: OpBarStyle,
) {
    let capped = cap_relative_to_native(values, stddevs, native);
    if !capped.main_indices.is_empty() {
        plot.add_trace(op_bar_trace(
            capped
                .main_indices
                .iter()
                .map(|&i| names[i].clone())
                .collect(),
            capped
                .main_indices
                .iter()
                .map(|&i| capped.display_values[i])
                .collect(),
            capped
                .main_indices
                .iter()
                .map(|&i| capped.display_stddevs[i])
                .collect(),
            capped
                .main_indices
                .iter()
                .map(|&i| colors[i].clone())
                .collect(),
            style.clone(),
        ));
    }
    if !capped.outlier_indices.is_empty() {
        let outline = style
            .line
            .clone()
            .unwrap_or_default()
            .color("#222")
            .width(2.0);
        plot.add_trace(
            op_bar_trace(
                capped
                    .outlier_indices
                    .iter()
                    .map(|&i| names[i].clone())
                    .collect(),
                capped
                    .outlier_indices
                    .iter()
                    .map(|&i| capped.display_values[i])
                    .collect(),
                capped
                    .outlier_indices
                    .iter()
                    .map(|&i| capped.display_stddevs[i])
                    .collect(),
                capped
                    .outlier_indices
                    .iter()
                    .map(|&i| colors[i].clone())
                    .collect(),
                OpBarStyle {
                    opacity: None,
                    line: Some(outline),
                    x_axis: style.x_axis,
                    y_axis: style.y_axis,
                    offset_group: style.offset_group,
                    alignment_group: style.alignment_group,
                },
            )
            .marker(
                plotly::common::Marker::new()
                    .color_array(
                        capped
                            .outlier_indices
                            .iter()
                            .map(|&i| {
                                color_with_alpha(&colors[i], style.opacity.unwrap_or(1.0) * 0.45)
                            })
                            .collect::<Vec<_>>(),
                    )
                    .line(plotly::common::Line::new().color("#222").width(2.0))
                    .pattern(
                        Pattern::new()
                            .shape(PatternShape::DiagonalCross)
                            .fill_mode(PatternFillMode::Overlay),
                    ),
            )
            .text_array(
                capped
                    .outlier_indices
                    .iter()
                    .map(|&i| capped.labels[i].clone())
                    .collect::<Vec<_>>(),
            )
            .text_position(TextPosition::Inside)
            .inside_text_font(Font::new().color("#111").size(13)),
        );
    }
}

fn color_with_alpha(color: &str, alpha: f64) -> String {
    let alpha = alpha.clamp(0.0, 1.0);
    if let Some(hex) = color.strip_prefix('#')
        && hex.len() == 6
        && let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&hex[0..2], 16),
            u8::from_str_radix(&hex[2..4], 16),
            u8::from_str_radix(&hex[4..6], 16),
        )
    {
        return format!("rgba({r}, {g}, {b}, {alpha:.3})");
    }
    color.to_string()
}

// ── Source-variant grouping ────────────────────────────────────────────────

const SOURCE_SUFFIXES: [&str; 3] = ["-base", "-stage", "-checkpoint"];

/// If `workload` ends in `-base`, `-stage`, or `-checkpoint`, return the
/// prefix (the group name). Otherwise `None`.
fn source_group_name(workload: &str) -> Option<&str> {
    for suffix in SOURCE_SUFFIXES {
        if let Some(prefix) = workload.strip_suffix(suffix) {
            return Some(prefix);
        }
    }
    None
}

/// Returns true if `name` has no source suffix itself but source-variant
/// workloads (e.g. `name-base`) exist in results. This identifies stale
/// entries from before the source axis was added.
fn is_stale_group_name(name: &str, results: &BenchResults) -> bool {
    if source_group_name(name).is_some() {
        return false; // Already a source variant, not a group name.
    }
    SOURCE_SUFFIXES.iter().any(|suffix| {
        let variant = format!("{name}{suffix}");
        results.workloads.iter().any(|w| w.workload == variant)
    })
}

pub fn normalize_legacy_workload_name(name: &str) -> String {
    let Some(suffix) = name
        .strip_suffix("-base")
        .or_else(|| name.strip_suffix("-stage"))
        .or_else(|| name.strip_suffix("-checkpoint"))
    else {
        return name.to_string();
    };

    match suffix {
        "meta-open-warm" => format!("meta-open{}", &name["meta-open-warm".len()..]),
        "meta-stat-warm" => format!("meta-stat{}", &name["meta-stat-warm".len()..]),
        "meta-readdir-warm" => format!("meta-readdir{}", &name["meta-readdir-warm".len()..]),
        _ => name.to_string(),
    }
}

fn source_pattern(label: &str) -> Pattern {
    match label {
        "base" => Pattern::new()
            .shape(PatternShape::None)
            .fill_mode(PatternFillMode::Replace),
        "stage" => Pattern::new()
            .shape(PatternShape::RightDiagonalLine)
            .fill_mode(PatternFillMode::Overlay)
            .solidity(0.4),
        "checkpoint" => Pattern::new()
            .shape(PatternShape::DiagonalCross)
            .fill_mode(PatternFillMode::Overlay)
            .solidity(0.4),
        _ => Pattern::new()
            .shape(PatternShape::None)
            .fill_mode(PatternFillMode::Replace),
    }
}

/// Render a grouped bar chart for all source variants of one operation.
/// E.g. for group "meta-append", renders meta-append-base, meta-append-stage,
/// meta-append-checkpoint side by side, with fill patterns encoding source.
fn render_grouped_op_workloads(
    group: &str,
    results: &BenchResults,
    out_dir: &Path,
    current_repo_state: Option<&RepoState>,
) -> Result<()> {
    let order = backends::display_order();
    let mut plot = Plot::new();

    let source_order = ["base", "stage", "checkpoint"];

    // Collect the full set of non-native backends across all source variants.
    let mut all_backend_names: Vec<String> = Vec::new();
    for &src in &source_order {
        let wl_name = format!("{group}-{src}");
        if let Some(wl) = results.workloads.iter().find(|w| w.workload == wl_name) {
            for b in &wl.backends {
                if b.backend != "native"
                    && report_backend_visible(&b.backend)
                    && !all_backend_names.contains(&b.backend)
                {
                    all_backend_names.push(b.backend.clone());
                }
            }
        }
    }
    all_backend_names
        .sort_by_key(|name| order.iter().position(|&n| n == name).unwrap_or(usize::MAX));

    // Look up the caveat for this workload group (if any).
    let caveat = source_order
        .iter()
        .find_map(|&src| workloads::caveat(&format!("{group}-{src}")));

    // Build one trace per source variant.
    for &src in &source_order {
        let wl_name = format!("{group}-{src}");
        let wl = results.workloads.iter().find(|w| w.workload == wl_name);

        let mut avg_lat_vals = Vec::with_capacity(all_backend_names.len());
        let mut stddev_vals = Vec::with_capacity(all_backend_names.len());
        let mut colors = Vec::with_capacity(all_backend_names.len());
        let mut texts = Vec::with_capacity(all_backend_names.len());
        let mut hover_texts = Vec::with_capacity(all_backend_names.len());
        let mut has_any = false;

        for backend_name in &all_backend_names {
            let result = wl.and_then(|w| w.backends.iter().find(|b| b.backend == *backend_name));
            match result {
                Some(b) => {
                    let lat = avg_latency_us(b.mean_iops.unwrap_or(0.0));
                    avg_lat_vals.push(lat);
                    stddev_vals.push(avg_latency_stddev_us(
                        b.mean_iops.unwrap_or(0.0),
                        b.stddev_iops.unwrap_or(0.0),
                    ));
                    colors.push(backend_color(backend_name).to_string());
                    texts.push(String::new());
                    let mut hover = format!(
                        "{backend_name} ({src})<br>{lat:.1} µs<br>{:.0} IOPS",
                        b.mean_iops.unwrap_or(0.0)
                    );
                    if let Some(ref c) = caveat
                        && backend_name != "native"
                    {
                        let c_html = c.replace('\n', "<br>");
                        hover.push_str(&format!("<br><br><i>{c_html}</i>"));
                    }
                    hover_texts.push(hover);
                    has_any = true;
                }
                None => {
                    avg_lat_vals.push(0.0);
                    stddev_vals.push(0.0);
                    colors.push("#ddd".to_string());
                    texts.push("N/A".to_string());
                    hover_texts.push(format!(
                        "{backend_name} ({src})<br>N/A — cannot flush kernel state without losing staging"
                    ));
                }
            }
        }

        if !has_any {
            continue;
        }

        let marker = plotly::common::Marker::new()
            .color_array(colors)
            .pattern(source_pattern(src));
        let mut trace = Bar::new(all_backend_names.clone(), avg_lat_vals)
            .name(src)
            .marker(marker)
            .hover_text_array(hover_texts)
            .hover_info(HoverInfo::Text)
            .error_y(
                ErrorData::new(ErrorType::Data)
                    .array(stddev_vals)
                    .visible(true),
            );
        if texts.iter().any(|t| !t.is_empty()) {
            trace = trace
                .text_array(texts)
                .text_position(TextPosition::Outside)
                .outside_text_font(Font::new().color("#999").size(11));
        }
        plot.add_trace(trace);
    }

    // Native baseline from any source variant (they should be ~identical).
    let native_iops = source_order.iter().find_map(|&src| {
        let wl_name = format!("{group}-{src}");
        results
            .workloads
            .iter()
            .find(|w| w.workload == wl_name)
            .and_then(|wl| {
                wl.backends
                    .iter()
                    .find(|b| b.backend == "native")
                    .and_then(|b| b.mean_iops)
            })
    });

    let mut layout = Layout::new()
        .bar_mode(BarMode::Group)
        .title(Title::with_text(group))
        .x_axis(Axis::new().title(Title::with_text("backend")))
        .y_axis(Axis::new().title(Title::with_text("avg latency (µs)")));

    if let Some(native) = native_iops {
        let native_lat = avg_latency_us(native);
        let shapes = vec![baseline_shape(native_lat, "y", "paper")];
        let annotations = vec![baseline_annotation(
            native_lat,
            "native baseline",
            "y",
            "paper",
            -10.0,
        )];
        add_native_baseline_hover_trace(
            &mut plot,
            &all_backend_names,
            native_lat,
            "native avg latency (µs)",
        );
        layout = layout.shapes(shapes).annotations(annotations);
    }

    plot.set_layout(layout);
    plot.set_configuration(Configuration::new().responsive(true).fill_frame(true));

    let html_path = out_dir.join(format!("report-{group}.html"));
    let mut full_html = plot.to_html();

    // Staleness: use the first available source variant.
    let status = source_order
        .iter()
        .find_map(|&src| {
            let wl_name = format!("{group}-{src}");
            results
                .workloads
                .iter()
                .find(|w| w.workload == wl_name)
                .map(|wl| workload_staleness(wl, current_repo_state))
        })
        .unwrap_or(WorkloadFreshness {
            kind: FreshnessKind::Unknown,
            summary: "unknown".to_string(),
            reasons: vec![],
            commit_label: None,
        });

    full_html = inject_workload_status(full_html, &status);

    std::fs::write(&html_path, full_html)
        .with_context(|| format!("writing {}", html_path.display()))?;
    eprintln!("Report written to {}", html_path.display());
    Ok(())
}

/// Generate an index page that embeds all per-workload reports as iframes,
/// grouped by micro/macro.
pub fn render_index(
    results: &BenchResults,
    out_dir: &Path,
    current_repo_state: Option<&RepoState>,
) -> Result<()> {
    // Look up kind and canonical order for each workload in results.
    let all_workloads = workloads::all();
    let order: Vec<&str> = all_workloads.iter().map(|w| w.name()).collect();
    let known: std::collections::HashMap<&str, WorkloadKind> =
        all_workloads.iter().map(|w| (w.name(), w.kind())).collect();

    let mut session_micros: Vec<String> = Vec::new();
    let mut session_macros: Vec<String> = Vec::new();
    let mut op_data: Vec<String> = Vec::new();
    let mut op_metadata: Vec<String> = Vec::new();
    let mut op_metadata_cold: Vec<String> = Vec::new();
    let mut op_other: Vec<String> = Vec::new();
    let mut seen_groups: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut seen_session: std::collections::HashSet<String> = std::collections::HashSet::new();
    for wl in &results.workloads {
        let canonical = normalize_legacy_workload_name(&wl.workload);
        // Skip stale entries superseded by source variants (e.g. "meta-rename"
        // when "meta-rename-base" exists). These aren't in the registered
        // workload list so they'd fall through to the wrong category.
        if is_stale_group_name(&canonical, results) {
            continue;
        }
        let kind = known.get(canonical.as_str()).copied().unwrap_or_else(|| {
            if canonical.starts_with("meta-") || canonical.starts_with("fio-") {
                WorkloadKind::Op
            } else {
                WorkloadKind::Micro
            }
        });
        match kind {
            WorkloadKind::Micro => {
                if seen_session.insert(canonical.clone()) {
                    session_micros.push(canonical);
                }
            }
            WorkloadKind::Macro => {
                if seen_session.insert(canonical.clone()) {
                    session_macros.push(canonical);
                }
            }
            WorkloadKind::Op => {
                // Deduplicate source variants into groups.
                let display_name = source_group_name(&canonical)
                    .map(|g| g.to_string())
                    .unwrap_or(canonical);
                if seen_groups.insert(display_name.clone()) {
                    if display_name.starts_with("meta-") {
                        if display_name.contains("-cold") {
                            op_metadata_cold.push(display_name);
                        } else {
                            op_metadata.push(display_name);
                        }
                    } else if display_name.starts_with("fio-") {
                        op_data.push(display_name);
                    } else {
                        op_other.push(display_name);
                    }
                }
            }
        }
    }

    // Sort by canonical registration order (use first source variant for groups).
    let pos = |name: &str| {
        // For groups like "meta-append", match against "meta-append-base".
        order
            .iter()
            .position(|&n| n == name || source_group_name(n) == Some(name))
            .unwrap_or(usize::MAX)
    };
    session_micros.sort_by_key(|n| pos(n));
    session_macros.sort_by_key(|n| pos(n));
    op_data.sort_by_key(|n| pos(n));
    op_metadata.sort_by_key(|n| pos(n));
    op_metadata_cold.sort_by_key(|n| pos(n));
    op_other.sort_by_key(|n| pos(n));

    let descriptions = workloads::descriptions();
    let e = &results.env;

    let mut html = String::new();
    html.push_str("<!DOCTYPE html>\n<html><head>\n");
    html.push_str("<meta charset=\"utf-8\">\n");
    html.push_str(&format!("<title>agfs-bench — {}</title>\n", e.hostname));
    html.push_str("<style>\n");
    html.push_str(
        "  body { font-family: system-ui, sans-serif; margin: 2em; background: #fafafa; }\n",
    );
    html.push_str("  h1 { font-size: 1.4em; }\n");
    html.push_str("  h2 { font-size: 1.1em; margin-top: 2em; color: #555; border-bottom: 1px solid #ddd; padding-bottom: 0.3em; }\n");
    html.push_str("  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(600px, 1fr)); gap: 1.2em; }\n");
    html.push_str("  .card { }\n");
    html.push_str("  .card-header { display:flex; align-items:center; gap:0.5em; flex-wrap:wrap; margin-bottom:0.35em; }\n");
    html.push_str("  .card-title { font-size: 0.95em; font-weight: 600; cursor: help; }\n");
    html.push_str("  .card-title .desc { font-weight: 400; color: #888; font-size: 0.72em; }\n");
    html.push_str("  .card-meta { display:flex; align-items:center; gap:0.45em; flex-wrap:wrap; margin-left:auto; }\n");
    html.push_str("  .card-stale details { display:inline-block; }\n");
    html.push_str("  .card-stale summary { cursor:pointer; color:#666; font-size:0.8em; }\n");
    html.push_str("  iframe { width: 100%; height: 420px; border: 1px solid #ddd; border-radius: 4px; background: #fff; }\n");
    html.push_str("  .env { font-size: 0.85em; color: #666; margin-bottom: 1.5em; }\n");
    html.push_str("  .env table { border-collapse: collapse; }\n");
    html.push_str("  .env td { padding: 0.15em 0; }\n");
    html.push_str(
        "  .env td:first-child { color: #999; padding-right: 1em; white-space: nowrap; }\n",
    );
    html.push_str("  .status-badge { display:inline-block; margin-left:0.5em; padding:0.1em 0.45em; border-radius:999px; font-size:0.78em; font-weight:600; }\n");
    html.push_str("  .status-fresh { color:#1f7a1f; background:#eaf7ea; }\n");
    html.push_str("  .status-stale { color:#b54a00; background:#fff1e8; }\n");
    html.push_str("  .status-unknown { color:#666; background:#f1f1f1; }\n");
    html.push_str("  details.cold-meta-collapsed { margin-top: 1.4em; }\n");
    html.push_str("  details.cold-meta-collapsed > summary { cursor: pointer; font-size: 1.02em; color: #555; border-bottom: 1px solid #ddd; padding-bottom: 0.3em; }\n");
    html.push_str("</style>\n");
    html.push_str("</head><body>\n");

    let title = match (&e.cloudlab_hardware, &e.cloudlab_cluster) {
        (Some(hw), Some(cluster)) => format!("{hw} @ {cluster}"),
        _ => e.hostname.clone(),
    };
    html.push_str(&format!("<h1>agfs-bench &mdash; {title}</h1>\n"));

    html.push_str("<div class=\"env\"><table>\n");
    if let (Some(hw), Some(cluster)) = (&e.cloudlab_hardware, &e.cloudlab_cluster) {
        html.push_str(&format!(
            "<tr><td>cloudlab</td><td>{hw} @ {cluster}</td></tr>\n"
        ));
    }
    html.push_str(&format!("<tr><td>host</td><td>{}</td></tr>\n", e.hostname));
    html.push_str(&format!("<tr><td>cpu</td><td>{}</td></tr>\n", e.cpu));
    html.push_str(&format!(
        "<tr><td>memory</td><td>{} GB</td></tr>\n",
        e.memory_gb
    ));
    html.push_str(&format!(
        "<tr><td>storage</td><td>{} &mdash; {} ({})</td></tr>\n",
        e.storage_device_model, e.storage, e.storage_device
    ));
    html.push_str(&format!(
        "<tr><td>filesystem</td><td>{} &mdash; {} GB total, {} GB free</td></tr>\n",
        e.filesystem, e.filesystem_size_gb, e.filesystem_free_gb
    ));
    html.push_str(&format!("<tr><td>kernel</td><td>{}</td></tr>\n", e.kernel));
    html.push_str(&format!("<tr><td>distro</td><td>{}</td></tr>\n", e.distro));
    if let Some(repo) = current_repo_state {
        html.push_str(&format!(
            "<tr><td>repo</td><td>{} (user: {}, kmod: {})</td></tr>\n",
            short_commit(&repo.commit),
            dirty_label(repo.user_dirty),
            dirty_label(repo.kmod_dirty)
        ));
    }
    html.push_str("<tr><td></td><td><a href=\"results.json\">results.json</a></td></tr>\n");
    html.push_str("</table></div>\n");

    // For grouped workloads (e.g. "meta-append"), resolve to the first
    // available source variant so description/details/freshness lookups work.
    let resolve_variant = |name: &str| -> Option<String> {
        // Try source suffixes first — these have details/descriptions.
        for suffix in SOURCE_SUFFIXES {
            let variant = format!("{name}{suffix}");
            if results.workloads.iter().any(|w| w.workload == variant) {
                return Some(variant);
            }
        }
        // Fall back to the name itself (non-grouped workloads).
        if results.workloads.iter().any(|w| w.workload == name) {
            return Some(name.to_string());
        }
        None
    };

    let emit_section = |html: &mut String, heading: &str, names: &[&str], h_tag: &str| {
        if names.is_empty() {
            return;
        }
        html.push_str(&format!("<{h_tag}>{heading}</{h_tag}>\n"));
        html.push_str("<div class=\"grid\">\n");
        for name in names {
            let variant = resolve_variant(name);
            let variant_name = variant.as_deref().unwrap_or(name);
            let freshness = results
                .workloads
                .iter()
                .find(|w| w.workload == variant_name)
                .map(|wl| workload_staleness(wl, current_repo_state))
                .unwrap_or(WorkloadFreshness {
                    kind: FreshnessKind::Unknown,
                    summary: "unknown".to_string(),
                    reasons: vec!["workload missing from results".to_string()],
                    commit_label: None,
                });
            let desc = descriptions
                .get(name)
                .or_else(|| descriptions.get(variant_name))
                .copied()
                .unwrap_or("");
            let escaped = desc.replace('"', "&quot;");
            let badge_class = match freshness.kind {
                FreshnessKind::Fresh => "status-badge status-fresh",
                FreshnessKind::Stale => "status-badge status-stale",
                FreshnessKind::Unknown => "status-badge status-unknown",
            };
            let freshness_html = if freshness.reasons.is_empty() {
                String::new()
            } else {
                let items = freshness
                    .reasons
                    .iter()
                    .map(|reason| format!("<li>{}</li>", escape_html(reason)))
                    .collect::<Vec<_>>()
                    .join("");
                format!(
                    "<div class=\"card-stale\"><details>\
                     <summary>why</summary>\
                     <ul style=\"margin:0.35em 0 0 1.2em;padding:0;color:#666;font-size:0.82em\">{items}</ul>\
                     </details></div>"
                )
            };
            let detail = workloads::details(name).or_else(|| workloads::details(variant_name));
            let detail_html = detail.map(|d| {
                format!(
                    "<details class=\"card-details\" style=\"margin:0 0 0.5em\">\
                     <summary style=\"cursor:pointer;color:#666;font-size:0.84em\">details</summary>\
                     <div style=\"margin-top:0.45em;font-size:0.84em;color:#555;line-height:1.4\">\
                     <p><strong>Summary:</strong> {summary}</p>\
                     <p><strong>Fixture:</strong> {fixture}</p>\
                     {harness}\
                     <p><strong>Execution:</strong> {execution}</p>\
                     <p><strong>Source:</strong> <code>{source}</code></p>\
                     </div></details>",
                    summary = escape_html(&d.summary),
                    fixture = escape_html(&d.fixture),
                    harness = d.harness.as_ref().map(|h| format!(
                        "<p><strong>Harness:</strong> {}</p>",
                        preserve_multiline_html(h)
                    )).unwrap_or_default(),
                    execution = preserve_multiline_html(&d.execution),
                    source = escape_html(&d.source_path),
                )
            }).unwrap_or_default();
            let hover_detail =
                workloads::details(name).or_else(|| workloads::details(variant_name));
            html.push_str(&format!(
                "  <div class=\"card\">\
                <div class=\"card-header\">\
                <div class=\"card-title\" title=\"{hover}\">{name} <span class=\"desc\">— {desc}</span></div>\
                <div class=\"card-meta\"><span class=\"{badge_class}\">{status}</span>{freshness_html}</div>\
                </div>\
                {detail_html}\
                <iframe src=\"report-{name}.html\"></iframe>\
                </div>\n",
                hover = hover_detail
                    .map(|d| escape_html(&d.summary))
                    .unwrap_or(escaped.clone()),
                badge_class = badge_class,
                status = escape_html(&badge_text(&freshness)),
                freshness_html = freshness_html,
            ));
        }
        html.push_str("</div>\n");
    };

    let op_data_refs: Vec<&str> = op_data.iter().map(|s| s.as_str()).collect();
    let op_metadata_refs: Vec<&str> = op_metadata.iter().map(|s| s.as_str()).collect();
    let op_metadata_cold_refs: Vec<&str> = op_metadata_cold.iter().map(|s| s.as_str()).collect();
    let op_other_refs: Vec<&str> = op_other.iter().map(|s| s.as_str()).collect();
    let session_micros_refs: Vec<&str> = session_micros.iter().map(|s| s.as_str()).collect();
    let session_macros_refs: Vec<&str> = session_macros.iter().map(|s| s.as_str()).collect();
    emit_section(
        &mut html,
        "Per-op Micro-benchmark: Big File Data Operations",
        &op_data_refs,
        "h2",
    );
    emit_section(
        &mut html,
        "Per-op Micro-benchmark: Small File Metadata Operations",
        &op_metadata_refs,
        "h2",
    );
    if !op_metadata_cold_refs.is_empty() {
        html.push_str(
            "<details class=\"cold-meta-collapsed\"><summary>Per-op Micro-benchmark: Small File Metadata Operations (Cold Cache)</summary>\n",
        );
        emit_section(&mut html, "", &op_metadata_cold_refs, "h3");
        html.push_str("</details>\n");
    }
    emit_section(
        &mut html,
        "Per-op Micro-benchmark: Other",
        &op_other_refs,
        "h2",
    );
    emit_section(
        &mut html,
        "Session Micro-benchmark (init/run/commit)",
        &session_micros_refs,
        "h2",
    );
    emit_section(
        &mut html,
        "Session Macro-benchmark",
        &session_macros_refs,
        "h2",
    );

    html.push_str("</body></html>\n");

    let index_path = out_dir.join("report.html");
    std::fs::write(&index_path, &html).context("writing index.html")?;
    eprintln!("Index written to {}", index_path.display());
    Ok(())
}

fn badge_text(freshness: &WorkloadFreshness) -> String {
    match freshness.kind {
        FreshnessKind::Fresh => freshness
            .commit_label
            .clone()
            .unwrap_or_else(|| freshness.summary.clone()),
        FreshnessKind::Stale | FreshnessKind::Unknown => match &freshness.commit_label {
            Some(commit) => format!("{} {}", freshness.summary, commit),
            None => freshness.summary.clone(),
        },
    }
}
