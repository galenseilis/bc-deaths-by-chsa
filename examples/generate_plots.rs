use serde::Deserialize;
use std::{collections::BTreeMap, error::Error, fs};

use plotters::prelude::*;

#[derive(Debug, Deserialize)]
struct Record {
    year: u64,
    chsa_code: u64,
    chsa_name: String,
    month: String,
    deaths: u64,
}

type TimeKey = (u64, u32);

fn month_number(month: &str) -> Result<u32, Box<dyn Error>> {
    let month = month.trim().to_lowercase();

    match month.as_str() {
        "january" | "jan" => Ok(1),
        "february" | "feb" => Ok(2),
        "march" | "mar" => Ok(3),
        "april" | "apr" => Ok(4),
        "may" => Ok(5),
        "june" | "jun" => Ok(6),
        "july" | "jul" => Ok(7),
        "august" | "aug" => Ok(8),
        "september" | "sep" | "sept" => Ok(9),
        "october" | "oct" => Ok(10),
        "november" | "nov" => Ok(11),
        "december" | "dec" => Ok(12),
        _ => Err(format!("unknown month: {month}").into()),
    }
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else if c.is_whitespace() {
                '_'
            } else {
                '-'
            }
        })
        .collect()
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut reader = csv::Reader::from_path("data/out/bc_deaths_by_chsa_long.csv")?;

    let output_dir = "plots";
    let chsa_output_dir = format!("{output_dir}/chsa");

    fs::create_dir_all(&chsa_output_dir)?;

    let mut bc_totals: BTreeMap<TimeKey, u64> = BTreeMap::new();

    let mut chsa_totals: BTreeMap<u64, (String, BTreeMap<TimeKey, u64>)> = BTreeMap::new();

    for result in reader.deserialize() {
        let record: Record = result?;

        let month = month_number(&record.month)?;
        let time = (record.year, month);

        *bc_totals.entry(time).or_default() += record.deaths;

        let (_, monthly_totals) = chsa_totals
            .entry(record.chsa_code)
            .or_insert_with(|| (record.chsa_name.clone(), BTreeMap::new()));

        *monthly_totals.entry(time).or_default() += record.deaths;
    }

    plot_bc_total(&bc_totals, output_dir)?;
    plot_chsa_totals(&chsa_totals, &chsa_output_dir)?;

    println!("Plots written to {output_dir}/");

    Ok(())
}

fn plot_bc_total(totals: &BTreeMap<TimeKey, u64>, output_dir: &str) -> Result<(), Box<dyn Error>> {
    if totals.is_empty() {
        return Ok(());
    }

    let path = format!("{output_dir}/bc_deaths_total.png");

    let root = BitMapBackend::new(&path, (2800, 1600)).into_drawing_area();

    root.fill(&WHITE)?;

    let areas = root.split_evenly((2, 1));

    let all_times: Vec<TimeKey> = totals.keys().copied().collect();

    let monthly: Vec<f64> = totals.values().map(|deaths| *deaths as f64).collect();

    let monthly_max = monthly.iter().copied().fold(0.0, f64::max);

    let mut cumulative_total = 0.0;

    let cumulative: Vec<f64> = monthly
        .iter()
        .map(|deaths| {
            cumulative_total += deaths;
            cumulative_total
        })
        .collect();

    let cumulative_max = cumulative.last().copied().unwrap_or(0.0);

    let x_max = (all_times.len() - 1) as f64;

    // ------------------------------------------------------------
    // Panel 1: monthly deaths
    // ------------------------------------------------------------

    let monthly_y_max = if monthly_max > 0.0 {
        monthly_max * 1.1
    } else {
        1.0
    };

    let mut monthly_chart = ChartBuilder::on(&areas[0])
        .caption("BC-wide Deaths Over Time", ("sans-serif", 48))
        .margin(40)
        .x_label_area_size(30)
        .y_label_area_size(100)
        .build_cartesian_2d(0.0..x_max, 0.0..monthly_y_max)?;

    monthly_chart
        .configure_mesh()
        .x_labels(12)
        .x_label_formatter(&|_| String::new())
        .y_desc("Deaths")
        .axis_desc_style(("sans-serif", 32))
        .label_style(("sans-serif", 24))
        .draw()?;

    let monthly_series = monthly
        .iter()
        .enumerate()
        .map(|(index, deaths)| (index as f64, *deaths));

    monthly_chart.draw_series(LineSeries::new(monthly_series, &BLUE))?;

    // ------------------------------------------------------------
    // Panel 2: cumulative deaths
    // ------------------------------------------------------------

    let cumulative_y_max = if cumulative_max > 0.0 {
        cumulative_max * 1.1
    } else {
        1.0
    };

    let mut cumulative_chart = ChartBuilder::on(&areas[1])
        .margin(40)
        .x_label_area_size(80)
        .y_label_area_size(100)
        .build_cartesian_2d(0.0..x_max, 0.0..cumulative_y_max)?;

    cumulative_chart
        .configure_mesh()
        .x_labels(12)
        .x_label_formatter(&|index| {
            let index = index.round() as usize;

            all_times
                .get(index)
                .map(|(year, month)| format!("{year}-{month:02}"))
                .unwrap_or_default()
        })
        .x_desc("Month")
        .y_desc("Cumulative deaths")
        .axis_desc_style(("sans-serif", 32))
        .label_style(("sans-serif", 24))
        .draw()?;

    let cumulative_series = cumulative
        .iter()
        .enumerate()
        .map(|(index, deaths)| (index as f64, *deaths));

    cumulative_chart.draw_series(LineSeries::new(cumulative_series, &RED))?;

    root.present()?;

    println!("Wrote {path}");

    Ok(())
}

fn plot_chsa_totals(
    totals: &BTreeMap<u64, (String, BTreeMap<TimeKey, u64>)>,
    output_dir: &str,
) -> Result<(), Box<dyn Error>> {
    for (chsa_code, (chsa_name, series)) in totals {
        if series.is_empty() {
            continue;
        }

        let safe_name = sanitize_filename(chsa_name);

        let path = format!("{output_dir}/{chsa_code}_{safe_name}.png");

        let root = BitMapBackend::new(&path, (2800, 1600)).into_drawing_area();

        root.fill(&WHITE)?;

        // Both panels share this exact x coordinate system.
        let areas = root.split_evenly((2, 1));

        let all_times: Vec<TimeKey> = series.keys().copied().collect();

        let monthly: Vec<f64> = all_times
            .iter()
            .map(|time| series.get(time).copied().unwrap_or(0) as f64)
            .collect();

        let monthly_max = monthly.iter().copied().fold(0.0, f64::max);

        let mut cumulative_total = 0.0;

        let cumulative: Vec<f64> = monthly
            .iter()
            .map(|deaths| {
                cumulative_total += deaths;
                cumulative_total
            })
            .collect();

        let cumulative_max = cumulative.last().copied().unwrap_or(0.0);

        let x_max = (all_times.len() - 1) as f64;

        // ------------------------------------------------------------
        // Panel 1: monthly deaths
        // ------------------------------------------------------------

        let monthly_y_max = if monthly_max > 0.0 {
            monthly_max * 1.1
        } else {
            1.0
        };

        let mut monthly_chart = ChartBuilder::on(&areas[0])
            .caption(format!("{chsa_name} ({chsa_code})"), ("sans-serif", 48))
            .margin(40)
            .x_label_area_size(30)
            .y_label_area_size(100)
            .build_cartesian_2d(0.0..x_max, 0.0..monthly_y_max)?;

        monthly_chart
            .configure_mesh()
            .x_labels(12)
            .x_label_formatter(&|_| String::new())
            .y_desc("Deaths")
            .axis_desc_style(("sans-serif", 32))
            .label_style(("sans-serif", 24))
            .draw()?;

        let monthly_series = monthly
            .iter()
            .enumerate()
            .map(|(index, deaths)| (index as f64, *deaths));

        monthly_chart.draw_series(LineSeries::new(monthly_series, &BLUE))?;

        // ------------------------------------------------------------
        // Panel 2: cumulative deaths
        // ------------------------------------------------------------

        let cumulative_y_max = if cumulative_max > 0.0 {
            cumulative_max * 1.1
        } else {
            1.0
        };

        let mut cumulative_chart = ChartBuilder::on(&areas[1])
            .margin(40)
            .x_label_area_size(80)
            .y_label_area_size(100)
            .build_cartesian_2d(0.0..x_max, 0.0..cumulative_y_max)?;

        cumulative_chart
            .configure_mesh()
            .x_labels(12)
            .x_label_formatter(&|index| {
                let index = index.round() as usize;

                all_times
                    .get(index)
                    .map(|(year, month)| format!("{year}-{month:02}"))
                    .unwrap_or_default()
            })
            .x_desc("Month")
            .y_desc("Cumulative deaths")
            .axis_desc_style(("sans-serif", 32))
            .label_style(("sans-serif", 24))
            .draw()?;

        let cumulative_series = cumulative
            .iter()
            .enumerate()
            .map(|(index, deaths)| (index as f64, *deaths));

        cumulative_chart.draw_series(LineSeries::new(cumulative_series, &RED))?;

        root.present()?;

        println!("Wrote {path}");
    }

    Ok(())
}
