// HTML report generation using plotly.

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

    let scenario_names: Vec<String> = wl.scenarios.iter().map(|s| s.scenario.clone()).collect();

    let native_ms = wl
        .scenarios
        .iter()
        .find(|s| s.scenario == "native")
        .map(|s| s.mean_total_ms);

    let staging_vals: Vec<f64> = wl
        .scenarios
        .iter()
        .map(|s| s.mean_staging_ms.unwrap_or(s.mean_total_ms))
        .collect();

    let commit_vals: Vec<f64> = wl
        .scenarios
        .iter()
        .map(|s| s.mean_commit_ms.unwrap_or(0.0))
        .collect();

    let stddev_vals: Vec<f64> = wl.scenarios.iter().map(|s| s.stddev_total_ms).collect();

    plot.add_trace(Bar::new(scenario_names.clone(), staging_vals).name("staging"));

    plot.add_trace(
        Bar::new(scenario_names.clone(), commit_vals)
            .name("commit")
            .error_y(
                ErrorData::new(ErrorType::Data)
                    .array(stddev_vals)
                    .visible(true),
            ),
    );

    // Draw the native reference line only across non-native scenarios so
    // that native itself renders as a normal bar.
    if let Some(native) = native_ms {
        let agfs_names: Vec<String> = scenario_names
            .iter()
            .filter(|s| s.as_str() != "native")
            .cloned()
            .collect();
        if !agfs_names.is_empty() {
            plot.add_trace(
                Scatter::new(agfs_names.clone(), vec![native; agfs_names.len()])
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
            .x_axis(Axis::new().title(Title::with_text("scenario")))
            .y_axis(Axis::new().title(Title::with_text("time (ms)"))),
    );
    plot.set_configuration(Configuration::new().responsive(true).fill_frame(true));

    let html_path = out_dir.join(format!("report-{}.html", wl.workload));
    plot.write_html(&html_path);
    eprintln!("Report written to {}", html_path.display());
    Ok(())
}
