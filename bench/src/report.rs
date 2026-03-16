// HTML report generation using plotly.

use crate::backends;
use crate::{BenchResults, WorkloadResult};
use anyhow::Result;
use plotly::common::{ErrorData, ErrorType, Mode, Title};
use plotly::layout::{Axis, BarMode};
use plotly::{Bar, Configuration, Layout, Plot, Scatter};
use std::path::Path;

pub fn render(results: &BenchResults, out_dir: &Path) -> Result<()> {
    for wl in &results.workloads {
        render_workload(wl, results, out_dir)?;
    }
    Ok(())
}

fn render_workload(wl: &WorkloadResult, results: &BenchResults, out_dir: &Path) -> Result<()> {
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

    let e = &results.env;
    let title = format!(
        "{} on {} — {} / {} {} / {} kernel",
        wl.workload, e.hostname, e.cpu, e.storage_device_model, e.filesystem, e.kernel,
    );

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
