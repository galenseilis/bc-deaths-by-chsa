use anyhow::{Context, Result, bail};
use chrono::{Datelike, Local};
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
struct Chsa {
    code: String,
    name: String,
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

    for year in args.from..=args.to {
        println!("== {year} ==");

        match process_year(&client, year, &args.pdf_dir, args.force) {
            Ok(records) => {
                println!("  extracted {} records", records.len());
                successful_years.push(year);
                all_records.extend(records);
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

fn process_year(
    client: &Client,
    year: u16,
    pdf_dir: &Path,
    force: bool,
) -> Result<Vec<DeathRecord>> {
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

fn parse_report(year: u16, pdf: &[u8]) -> Result<Vec<DeathRecord>> {
    let text = extract_text_from_mem(pdf)
        .with_context(|| format!("could not extract PDF text for {year}"))?;

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

    Ok(records)
}

/// pdf-extract separates pages with form-feed characters in these reports.
fn split_pages(text: &str) -> Vec<String> {
    text.split('\x0c')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(ToOwned::to_owned)
        .collect()
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

    /*
     * Usually numbers starts with the annual Total column:
     *
     *     Total[0..n]
     *     Jan[0..n]
     *     Feb[0..n]
     *     ...
     *
     * Some report revisions put the monthly columns first.
     *
     * Determine which layout we have by looking for a plausible
     * annual-total relationship.
     */
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

            // If offset == n, compare the first block to the sum.
            if offset == n {
                let annual_total = numbers[row];

                if total == annual_total {
                    matches += 1;
                }
            }
        }

        if offset == 0 {
            // No annual-total block. Give this candidate a neutral score.
            matches = 1;
        }

        if best.map_or(true, |(_, score)| matches > score) {
            best = Some((offset, matches));
        }
    }

    best.map(|(offset, _)| offset)
        .context("could not determine monthly column position")
}

fn parse_chsa(line: &str) -> Option<Chsa> {
    let mut parts = line.splitn(2, char::is_whitespace);

    let code = parts.next()?;

    if code.len() != 4 || !code.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }

    let name = parts.next()?.trim();

    if name.is_empty() {
        return None;
    }

    Some(Chsa {
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
