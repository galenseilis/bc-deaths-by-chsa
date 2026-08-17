use anyhow::{Context, Result, bail};
use chrono::{Datelike, Local, NaiveDate};
use clap::Parser;
use csv::Writer;
use pdf_extract::extract_text_from_mem;
use reqwest::blocking::Client;
use serde::Serialize;
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

const BASE_URL: &str = "https://www2.gov.bc.ca/assets/gov/birth-adoption-death-marriage-and-divorce/statistics-reports/death-reports";

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn current_year() -> u16 {
    Local::now().year() as u16
}

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    /// First report year to download.
    #[arg(long, default_value_t = 2007)]
    from: u16,

    /// Last report year to download.
    #[arg(long, default_value_t = current_year())]
    to: u16,

    /// Directory in which PDFs are cached.
    #[arg(long, default_value = "data/pdfs")]
    pdf_dir: PathBuf,

    /// Directory for generated CSV files.
    #[arg(long, default_value = "data/out")]
    output_dir: PathBuf,

    /// Re-download PDFs even if they already exist locally.
    #[arg(long)]
    force: bool,

    /// Also write a wide-format CSV.
    #[arg(long)]
    wide: bool,
}

#[derive(Debug, Serialize, Clone)]
struct DeathRecord {
    year: u16,
    chsa_code: String,
    chsa_name: String,
    month: String,
    deaths: u32,
}

#[derive(Debug, Clone)]
struct CHSA {
    code: String,
    name: String,
}

#[derive(Debug)]
struct ParsedReport {
    reporting_date: NaiveDate,
    records: Vec<DeathRecord>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    if args.from > args.to {
        bail!("--from must not be greater than --to");
    }

    fs::create_dir_all(&args.pdf_dir)?;
    fs::create_dir_all(&args.output_dir)?;

    let client = Client::builder()
        .timeout(Duration::from_secs(60))
        .user_agent("bc-deaths-by-chsa/0.1")
        .build()?;

    let mut all_records = Vec::new();
    let mut successful_years = Vec::new();
    let mut missing_years = Vec::new();

    let mut latest_reporting_date: Option<NaiveDate> = None;

    for year in args.from..=args.to {
        println!("== {year} ==");

        match process_year(&client, year, &args.pdf_dir, args.force) {
            Ok(report) => {
                println!("  reporting date: {}", report.reporting_date);
                println!("  extracted {} records", report.records.len());

                latest_reporting_date = Some(
                    latest_reporting_date
                        .map(|date| date.max(report.reporting_date))
                        .unwrap_or(report.reporting_date),
                );

                successful_years.push(year);
                all_records.extend(report.records);
            }
            Err(err) => {
                eprintln!("  skipped {year}: {err:#}");
                missing_years.push(year);
            }
        }
    }

    if all_records.is_empty() {
        bail!("No reports could be parsed");
    }

    let latest_reporting_date = latest_reporting_date.context("No reporting dates were found")?;

    println!();
    println!("Latest reporting date: {latest_reporting_date}");

    /*
     * The current report contains the current month and all remaining
     * months of the year as zero-valued placeholders.
     *
     * Therefore, for historical data, retain only months strictly
     * before the reporting month.
     *
     * Example:
     *
     *     reporting date = 2026-08-01
     *
     *     retain through = 2026-07
     *
     *     discard       = 2026-08 through 2026-12
     */
    let cutoff = (latest_reporting_date.year(), latest_reporting_date.month());

    let before_filter = all_records.len();

    all_records.retain(|record| {
        let record_month = month_number(&record.month) as u32;
        (record.year as i32, record_month) < cutoff
    });

    let removed = before_filter - all_records.len();

    println!(
        "Removed {removed} records at or after the reporting month \
         (historical cutoff: {}-{:#02})",
        cutoff.0, cutoff.1
    );

    // Stable ordering is useful for reproducible datasets.
    all_records.sort_by(|a, b| {
        (a.year, &a.chsa_code, month_number(&a.month)).cmp(&(
            b.year,
            &b.chsa_code,
            month_number(&b.month),
        ))
    });

    let long_path = args.output_dir.join("bc_deaths_by_chsa_long.csv");
    write_long_csv(&long_path, &all_records)?;

    println!();
    println!("Wrote {}", long_path.display());

    if args.wide {
        let wide_path = args.output_dir.join("bc_deaths_by_chsa_wide.csv");
        write_wide_csv(&wide_path, &all_records)?;
        println!("Wrote {}", wide_path.display());
    }

    println!();
    println!("Successful years: {:?}", successful_years);

    if !missing_years.is_empty() {
        println!("Unavailable/failed years: {:?}", missing_years);
    }

    Ok(())
}

fn process_year(client: &Client, year: u16, pdf_dir: &Path, force: bool) -> Result<ParsedReport> {
    let filename = format!("deaths-by-chsa-{year}.pdf");
    let path = pdf_dir.join(&filename);

    if !path.exists() || force {
        let url = format!("{BASE_URL}/{filename}");

        println!("  downloading {url}");

        let response = client
            .get(&url)
            .send()
            .with_context(|| format!("request failed for {url}"))?;

        if !response.status().is_success() {
            bail!("HTTP {}", response.status());
        }

        let bytes = response.bytes().context("failed reading PDF response")?;

        if bytes.len() < 1000 || !bytes.starts_with(b"%PDF") {
            bail!("response does not appear to be a PDF");
        }

        fs::write(&path, &bytes).with_context(|| format!("failed writing {}", path.display()))?;
    } else {
        println!("  using cached {}", path.display());
    }

    let bytes = fs::read(&path)?;
    parse_report(year, &bytes)
}

fn parse_report(year: u16, pdf: &[u8]) -> Result<ParsedReport> {
    let text = extract_text_from_mem(pdf)
        .with_context(|| format!("could not extract PDF text for {year}"))?;

    let reporting_date = extract_reporting_date(year, &text)?;

    let pages = split_pages(&text);

    if pages.is_empty() {
        bail!("PDF contains no extractable pages");
    }

    let mut records = Vec::new();

    for (page_no, page) in pages.iter().enumerate() {
        let page_records = parse_page(year, page)
            .with_context(|| format!("failed parsing page {}", page_no + 1))?;

        records.extend(page_records);
    }

    if records.is_empty() {
        bail!("no CHSA records found");
    }

    // The PDF contains the same CHSA on multiple pages only if the
    // table spills over. Avoid accidental duplication.
    let mut seen = HashSet::new();

    records.retain(|r| seen.insert((r.year, r.chsa_code.clone(), r.month.clone())));

    Ok(ParsedReport {
        reporting_date,
        records,
    })
}

/// Extract the report's reporting date from the PDF text.
///
/// The report date is expected to occur near text such as:
///
///     Reporting Date: August 1, 2026
///
/// or:
///
///     Report Date August 01 2026
///
/// We deliberately only consider lines containing date/reporting
/// terminology so that dates elsewhere in the PDF are not accidentally
/// interpreted as the reporting date.
fn extract_reporting_date(year: u16, text: &str) -> Result<NaiveDate> {
    for raw_line in text.lines() {
        let line = normalize_line(raw_line);
        let lower = line.to_lowercase();

        let looks_like_reporting_date = lower.contains("reporting date")
            || lower.contains("report date")
            || lower.contains("reporting as of")
            || lower.contains("data as of")
            || lower.contains("as of");

        if !looks_like_reporting_date {
            continue;
        }

        if let Some(date) = parse_date_from_line(&line) {
            return Ok(date);
        }
    }

    bail!(
        "could not find reporting date in {year} PDF; \
         inspect the extracted PDF text for the date format"
    )
}

/// Look for a conventional month/day/year date in a line.
///
/// Handles forms such as:
///
///     August 1, 2026
///     August 01 2026
///     Aug 1 2026
///
/// We also accept ISO-style dates:
///
///     2026-08-01
fn parse_date_from_line(line: &str) -> Option<NaiveDate> {
    let cleaned = line.replace(',', " ").replace('/', " ").replace('-', " ");

    let tokens: Vec<&str> = cleaned.split_whitespace().collect();

    for i in 0..tokens.len() {
        // Month-name form:
        //
        //     August 1 2026
        //     Aug 1 2026
        if let Some(month) = month_name_number(tokens[i]) {
            if i + 2 < tokens.len() {
                let day = tokens[i + 1].parse::<u32>().ok()?;
                let year = tokens[i + 2].parse::<i32>().ok()?;

                if (1900..=2100).contains(&year) && (1..=31).contains(&day) {
                    if let Some(date) = NaiveDate::from_ymd_opt(year, month, day) {
                        return Some(date);
                    }
                }
            }
        }

        // Numeric form after replacement:
        //
        //     2026 08 01
        //
        // or:
        //
        //     08 01 2026
        if i + 2 < tokens.len() {
            let a = tokens[i].parse::<u32>().ok();
            let b = tokens[i + 1].parse::<u32>().ok();
            let c = tokens[i + 2].parse::<u32>().ok();

            if let (Some(a), Some(b), Some(c)) = (a, b, c) {
                // YYYY MM DD
                if (1900..=2100).contains(&(a as i32)) {
                    if let Some(date) = NaiveDate::from_ymd_opt(a as i32, b, c) {
                        return Some(date);
                    }
                }

                // MM DD YYYY
                if (1900..=2100).contains(&(c as i32)) {
                    if let Some(date) = NaiveDate::from_ymd_opt(c as i32, a, b) {
                        return Some(date);
                    }
                }
            }
        }
    }

    None
}

fn month_name_number(month: &str) -> Option<u32> {
    match month.trim().to_lowercase().as_str() {
        "january" | "jan" => Some(1),
        "february" | "feb" => Some(2),
        "march" | "mar" => Some(3),
        "april" | "apr" => Some(4),
        "may" => Some(5),
        "june" | "jun" => Some(6),
        "july" | "jul" => Some(7),
        "august" | "aug" => Some(8),
        "september" | "sep" | "sept" => Some(9),
        "october" | "oct" => Some(10),
        "november" | "nov" => Some(11),
        "december" | "dec" => Some(12),
        _ => None,
    }
}

/// Split extracted PDF text into per-page segments.
///
/// Ideally `pdf-extract` separates pages with form-feed characters, but
/// some versions (and some PDFs) omit them.  When that happens we fall
/// back to splitting on the `Page  X of  Y` headers that every BC
/// report contains.
fn split_pages(text: &str) -> Vec<String> {
    // Fast path: form-feed separated pages.
    let pages: Vec<String> = text
        .split('\x0c')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(ToOwned::to_owned)
        .collect();

    if pages.len() > 1 {
        return pages;
    }

    // Fallback: split on "Page  N of  M" header lines.
    split_on_page_markers(text)
}

fn split_on_page_markers(text: &str) -> Vec<String> {
    // Collect the byte offset of every line that looks like "Page  N of  M".
    let mut page_starts: Vec<usize> = Vec::new();
    let mut offset = 0usize;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Page ") && trimmed.contains(" of ") {
            page_starts.push(offset);
        }
        offset += line.len() + 1; // +1 for the '\n'
    }

    if page_starts.len() < 2 {
        return vec![text.to_owned()];
    }

    let mut pages = Vec::new();

    for window in page_starts.windows(2) {
        let segment = text[window[0]..window[1]].trim().to_owned();
        if !segment.is_empty() {
            pages.push(segment);
        }
    }

    // Last page.
    let last = text[*page_starts.last().unwrap()..].trim().to_owned();
    if !last.is_empty() {
        pages.push(last);
    }

    pages
}

/// Parse one PDF page.
///
/// The BC reports are laid out visually as:
///
///     CHSA | Jan | Feb | ... | Dec | Total
///
/// but PDF text extraction generally emits the columns vertically:
///
///     CHSA names...
///     totals...
///     Jan...
///     Feb...
///     ...
///
/// Therefore we recover the table by identifying the CHSA-name block
/// and then consuming twelve numeric blocks of the same length.
fn parse_page(year: u16, page: &str) -> Result<Vec<DeathRecord>> {
    let lines: Vec<String> = page
        .lines()
        .map(normalize_line)
        .filter(|s| !s.is_empty())
        .collect();

    // Locate the first actual CHSA row.
    let first_chsa = lines
        .iter()
        .position(|line| parse_chsa(line).is_some())
        .context("could not find CHSA rows")?;

    let mut chsas = Vec::new();
    let mut i = first_chsa;

    while i < lines.len() {
        if let Some(chsa) = parse_chsa(&lines[i]) {
            chsas.push(chsa);
            i += 1;
        } else {
            break;
        }
    }

    if chsas.is_empty() {
        bail!("empty CHSA block");
    }

    let n = chsas.len();

    // From this point forward we expect numeric blocks.
    let mut numbers = Vec::<u32>::new();

    for line in &lines[i..] {
        // Stop before the provincial total / footer.
        if line.starts_with("Provincial Total")
            || line.starts_with("DTM040")
            || line.starts_with("Deaths are assigned")
        {
            break;
        }

        // Ignore column headings.
        if MONTHS.contains(&line.as_str())
            || line == "Community Health Service Area"
            || line == "Total"
        {
            continue;
        }

        if let Some(n) = parse_number(line) {
            numbers.push(n);
        }
    }

    /*
     * Depending on the report version, the extracted order can include
     * an annual Total column either before or after the monthly columns.
     *
     * We only need Jan-Dec. The most reliable invariant is that there
     * are at least 12*n monthly values, with each month containing n
     * values.
     */
    if numbers.len() < 12 * n {
        bail!(
            "expected at least {} numeric cells for {} CHSAs, found {}",
            12 * n,
            n,
            numbers.len()
        );
    }

    let monthly_start = detect_monthly_start(&numbers, n)?;

    let mut out = Vec::with_capacity(n * 12);

    for month_index in 0..12 {
        let start = monthly_start + month_index * n;
        let end = start + n;

        if end > numbers.len() {
            bail!("monthly block extends beyond extracted numbers");
        }

        for (row, deaths) in numbers[start..end].iter().enumerate() {
            out.push(DeathRecord {
                year,
                chsa_code: chsas[row].code.clone(),
                chsa_name: chsas[row].name.clone(),
                month: MONTHS[month_index].to_string(),
                deaths: *deaths,
            });
        }
    }

    Ok(out)
}

/// Detect whether the first numeric block is the annual Total.
///
/// The annual total should normally equal the sum of Jan-Dec. We test
/// both possible offsets and choose the one that produces the strongest
/// agreement.
fn detect_monthly_start(numbers: &[u32], n: usize) -> Result<usize> {
    let candidates = [0usize, n];

    let mut best: Option<(usize, usize)> = None;

    for &offset in &candidates {
        if numbers.len() < offset + 12 * n {
            continue;
        }

        let mut matches = 0;

        for row in 0..n {
            let total: u32 = (0..12).map(|month| numbers[offset + month * n + row]).sum();

            if offset == n {
                let annual_total = numbers[row];

                if total == annual_total {
                    matches += 1;
                }
            }
        }

        if offset == 0 {
            matches = 1;
        }

        if best.map_or(true, |(_, score)| matches > score) {
            best = Some((offset, matches));
        }
    }

    best.map(|(offset, _)| offset)
        .context("could not determine monthly column position")
}

fn parse_chsa(line: &str) -> Option<CHSA> {
    // The reports include an "Unknown CHSA" row for deaths whose
    // residential postal code could not be mapped to a CHSA.
    if line.starts_with("Unknown CHSA") {
        return Some(CHSA {
            code: "UNKN".to_string(),
            name: "Unknown CHSA".to_string(),
        });
    }

    let mut parts = line.splitn(2, char::is_whitespace);

    let code = parts.next()?;

    if code.len() != 4 || !code.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }

    let name = parts.next()?.trim();

    if name.is_empty() {
        return None;
    }

    Some(CHSA {
        code: code.to_string(),
        name: name.to_string(),
    })
}

fn parse_number(s: &str) -> Option<u32> {
    let cleaned = s.trim().replace(',', "");

    if cleaned.chars().all(|c| c.is_ascii_digit()) {
        cleaned.parse().ok()
    } else {
        None
    }
}

fn normalize_line(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn month_number(month: &str) -> u8 {
    MONTHS
        .iter()
        .position(|m| *m == month)
        .map(|n| n as u8 + 1)
        .unwrap_or(0)
}

fn write_long_csv(path: &Path, records: &[DeathRecord]) -> Result<()> {
    let mut writer = Writer::from_path(path)?;

    for record in records {
        writer.serialize(record)?;
    }

    writer.flush()?;
    Ok(())
}

#[derive(Debug, Serialize)]
struct WideRecord<'a> {
    year: u16,
    chsa_code: &'a str,
    chsa_name: &'a str,
    jan: u32,
    feb: u32,
    mar: u32,
    apr: u32,
    may: u32,
    jun: u32,
    jul: u32,
    aug: u32,
    sep: u32,
    oct: u32,
    nov: u32,
    dec: u32,
    total: u32,
}

fn write_wide_csv(path: &Path, records: &[DeathRecord]) -> Result<()> {
    use std::collections::BTreeMap;

    let mut grouped: BTreeMap<(u16, String, String), [u32; 12]> = BTreeMap::new();

    for r in records {
        let month = month_number(&r.month);

        if month == 0 {
            continue;
        }

        grouped
            .entry((r.year, r.chsa_code.clone(), r.chsa_name.clone()))
            .or_insert([0; 12])[(month - 1) as usize] = r.deaths;
    }

    let mut writer = Writer::from_path(path)?;

    for ((year, code, name), months) in grouped {
        let total = months.iter().sum();

        writer.serialize(WideRecord {
            year,
            chsa_code: &code,
            chsa_name: &name,
            jan: months[0],
            feb: months[1],
            mar: months[2],
            apr: months[3],
            may: months[4],
            jun: months[5],
            jul: months[6],
            aug: months[7],
            sep: months[8],
            oct: months[9],
            nov: months[10],
            dec: months[11],
            total,
        })?;
    }

    writer.flush()?;
    Ok(())
}
