// HTML report generation using plotly.

use crate::backends;
use crate::workload::WorkloadKind;
use crate::workloads;
use crate::{BenchResults, WorkloadResult};
use anyhow::{Context, Result};
use plotly::common::{ErrorData, ErrorType, Mode, Title};
use plotly::layout::{Axis, BarMode};
use plotly::{Bar, Configuration, Layout, Plot, Scatter};
use std::path::Path;

pub fn render(results: &BenchResults, out_dir: &Path) -> Result<()> {
    for wl in &results.workloads {
        render_workload(wl, results, out_dir)?;
    }
    render_index(results, out_dir)?;
    Ok(())
}

fn render_workload(wl: &WorkloadResult, _results: &BenchResults, out_dir: &Path) -> Result<()> {
    let mut plot = Plot::new();

    // Sort backends into canonical display order; unknown backends go at the end.
    let order = backends::display_order();
    let mut sorted = wl.backends.clone();
    sorted.sort_by_key(|b| {
        order
            .iter()
            .position(|&name| name == b.backend)
            .unwrap_or(usize::MAX)
    });

    let backend_names: Vec<String> = sorted.iter().map(|b| b.backend.clone()).collect();

    let native_ms = sorted
        .iter()
        .find(|b| b.backend == "native")
        .map(|b| b.mean_total_ms);

    let init_vals: Vec<f64> = sorted
        .iter()
        .map(|b| b.mean_init_ms.unwrap_or(0.0))
        .collect();

    // For native (no phases), the full time goes into staging.
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

    // Draw the native reference line only across non-native backends so
    // that native itself renders as a normal bar.
    if let Some(native) = native_ms {
        let other_names: Vec<String> = backend_names
            .iter()
            .filter(|s| s.as_str() != "native")
            .cloned()
            .collect();
        if !other_names.is_empty() {
            plot.add_trace(
                Scatter::new(other_names.clone(), vec![native; other_names.len()])
                    .name("native baseline")
                    .mode(Mode::Lines),
            );
        }
    }

    let title = wl.workload.clone();

    plot.set_layout(
        Layout::new()
            .bar_mode(BarMode::Stack)
            .title(Title::with_text(title))
            .x_axis(Axis::new().title(Title::with_text("backend")))
            .y_axis(Axis::new().title(Title::with_text("time (ms)"))),
    );
    plot.set_configuration(Configuration::new().responsive(true).fill_frame(true));

    let html_path = out_dir.join(format!("report-{}.html", wl.workload));
    plot.write_html(&html_path);
    eprintln!("Report written to {}", html_path.display());
    Ok(())
}

/// Generate an index page that embeds all per-workload reports as iframes,
/// grouped by micro/macro.
fn render_index(results: &BenchResults, out_dir: &Path) -> Result<()> {
    // Look up kind and canonical order for each workload in results.
    let all_workloads = workloads::all();
    let order: Vec<&str> = all_workloads.iter().map(|w| w.name()).collect();
    let known: std::collections::HashMap<&str, WorkloadKind> =
        all_workloads.iter().map(|w| (w.name(), w.kind())).collect();

    let mut micros: Vec<&str> = Vec::new();
    let mut macros: Vec<&str> = Vec::new();
    for wl in &results.workloads {
        let kind = known
            .get(wl.workload.as_str())
            .copied()
            .unwrap_or(WorkloadKind::Micro);
        match kind {
            WorkloadKind::Micro => micros.push(&wl.workload),
            WorkloadKind::Macro => macros.push(&wl.workload),
        }
    }

    // Sort by canonical registration order.
    let pos = |name: &str| order.iter().position(|&n| n == name).unwrap_or(usize::MAX);
    micros.sort_by_key(|n| pos(n));
    macros.sort_by_key(|n| pos(n));

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
    html.push_str("  .card-label { font-size: 0.95em; font-weight: 600; margin-bottom: 0.3em; cursor: help; }\n");
    html.push_str("  .card-label .desc { font-weight: 400; color: #888; font-size: 0.85em; }\n");
    html.push_str("  iframe { width: 100%; height: 420px; border: 1px solid #ddd; border-radius: 4px; background: #fff; }\n");
    html.push_str("  .env { font-size: 0.85em; color: #666; margin-bottom: 1.5em; }\n");
    html.push_str("  .env table { border-collapse: collapse; }\n");
    html.push_str("  .env td { padding: 0.15em 0; }\n");
    html.push_str("  .env td:first-child { color: #999; padding-right: 1em; white-space: nowrap; }\n");
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
    html.push_str(&format!(
        "<tr><td>host</td><td>{}</td></tr>\n", e.hostname
    ));
    html.push_str(&format!(
        "<tr><td>cpu</td><td>{}</td></tr>\n", e.cpu
    ));
    html.push_str(&format!(
        "<tr><td>memory</td><td>{} GB</td></tr>\n", e.memory_gb
    ));
    html.push_str(&format!(
        "<tr><td>storage</td><td>{} &mdash; {} ({})</td></tr>\n",
        e.storage_device_model, e.storage, e.storage_device
    ));
    html.push_str(&format!(
        "<tr><td>filesystem</td><td>{} &mdash; {} GB total, {} GB free</td></tr>\n",
        e.filesystem, e.filesystem_size_gb, e.filesystem_free_gb
    ));
    html.push_str(&format!(
        "<tr><td>kernel</td><td>{}</td></tr>\n", e.kernel
    ));
    html.push_str(&format!(
        "<tr><td>distro</td><td>{}</td></tr>\n", e.distro
    ));
    html.push_str(&format!(
        "<tr><td></td><td><a href=\"results.json\">results.json</a></td></tr>\n"
    ));
    html.push_str("</table></div>\n");

    let emit_section = |html: &mut String, heading: &str, names: &[&str]| {
        if names.is_empty() {
            return;
        }
        html.push_str(&format!("<h2>{heading}</h2>\n"));
        html.push_str("<div class=\"grid\">\n");
        for name in names {
            let desc = descriptions.get(name).copied().unwrap_or("");
            let escaped = desc.replace('"', "&quot;");
            html.push_str(&format!(
                "  <div class=\"card\">\
                <div class=\"card-label\" title=\"{escaped}\">{name} \
                <span class=\"desc\">— {desc}</span></div>\
                <iframe src=\"report-{name}.html\"></iframe>\
                </div>\n"
            ));
        }
        html.push_str("</div>\n");
    };

    emit_section(&mut html, "Microbenchmarks", &micros);
    emit_section(&mut html, "Macrobenchmarks", &macros);

    html.push_str("</body></html>\n");

    let index_path = out_dir.join("report.html");
    std::fs::write(&index_path, &html).context("writing index.html")?;
    eprintln!("Index written to {}", index_path.display());
    Ok(())
}
