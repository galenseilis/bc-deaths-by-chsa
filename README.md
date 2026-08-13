# BC Deaths by Community Health Service Area

A Rust command-line tool that downloads British Columbia's annual death reports, extracts monthly death counts by Community Health Service Area (CHSA), and converts the results into CSV datasets.

The tool retrieves reports published by the Government of British Columbia, caches the source PDFs locally, parses their tabular contents, and produces either a long-format CSV or an optional wide-format CSV.

## Features

- Downloads BC annual death reports for a configurable year range.
- Caches PDFs locally to avoid unnecessary downloads.
- Re-downloads reports with `--force` when required.
- Extracts monthly death counts for each CHSA.
- Handles the column ordering produced by PDF text extraction.
- Removes duplicate CHSA/month records.
- Produces deterministic, chronologically sorted output.
- Continues processing when an individual year's report is unavailable or cannot be parsed.
- Optionally produces a wide-format dataset.

## Requirements

- Rust and Cargo
- Internet access when reports need to be downloaded.

## Installation


```bash
git clone  https://github.com/galenseilis/bc-deaths-by-chsa.git
cd <repository_directory>  
cargo build --release
```

The resulting executable will be located at `./target/release/bc-deaths-by-chsa`.

You can also run it directly with Cargo during development: `cargo run --release`.

## Usage

By default, the program:

- starts at 2007.
- ends at the current year.
- catches PDFs in `data/pdfs`.
- writes CSV output to `data/out`.

### Specify a year range

Download and process reports from 2010 through 2020:

```bash
cargo run --release -- --from 2010 --to 2020
```

### Force re-downloads

By default, existing PDFs are reused. To download them again:

```bash
cargo run --release -- --from 2009 --to 2020 --force
```

### Customize directories

Specify where source PDFs and generated CSV files should be stored:

```bash
cargo run --release -- \
    --pdf-dir ./pdfs \
    --output-dir ./output
```

### Generate the wide-format CSV

The long-format CSV is always generated. Add `--wide` to also generate a wide-format file:

```bash
cargo run --release -- --from 2010 --to 2020 --wide
```

## Command-line options

| Option | Default | Description |
| :--- | :--- | :--- |
| `--from` | 2007 | First report year to download |
| `--to` | Current year | Last report year to download |
| `--pdf-dir` | data/pdfs | Directory used to cache PDFs |
| `--output-dir` | data/out | Directory for generated CSV files |
| `--force` | Off | Re-download PDFs even when cached |
| `--wide` | Off | Also generate the wide-format CSV |
| `--help` | — | Display command-line help |
| `--version` | — | Display the application version |

For example: `cargo run --release -- --help`.

## Output

### Long format

The default output is:

```text
data/out/bc_deaths_by_chsa_long.csv
```

It contains one row per year, CHSA, and month:

```text
year,chsa_code,chsa_name,month,deaths
2020,1001,Example CHSA,Jan,12
2020,1001,Example CHSA,Feb,10
2020,1001,Example CHSA,Mar,14
```

The columns are:

- year — report year
- chsa_code — four-digit CHSA code
- chsa_name — CHSA name
- month — three-letter month abbreviation
- deaths — number of deaths reported for that CHSA and month

This format is convenient for analysis with SQL, pandas, R, or other data-processing tools.

### Wide format

With `--wide`, the program additionally creates:

```text
data/out/bc_deaths_by_chsa_wide.csv
```

Each row represents one CHSA/year combination:

```text
year,chsa_code,chsa_name,jan,feb,mar,apr,may,jun,jul,aug,sep,oct,nov,dec,total
2020,1001,Example CHSA,12,10,14,11,13,9,10,12,8,15,11,10,135
```

The `total` column is calculated by summing the twelve monthly values.

## Data source

The source data consists of annual death reports published by the Government of British Columbia.

The application constructs report URLs using the following pattern:

```text
https://www2.gov.bc.ca/assets/gov/birth-adoption-death-marriage-and-divorce/statistics-reports/death-reports/deaths-by-chsa-YYYY.pdf
```

For example: `deaths-by-chsa-2020.pdf`.

The application validates downloaded responses to ensure they appear to be PDF files before saving them.

## How parsing works

The reports are PDFs rather than conventional CSV or spreadsheet files. PDF text extraction does not necessarily preserve the visual table layout.

The reports visually resemble:

```text
Community Health Service Area | Jan | Feb | ... | Dec | Total
```

but extracted PDF text can instead arrange the values into vertical blocks.

The parser therefore:

- Extracts text from the PDF.
- Splits the extracted text into pages.
- Locates the CHSA rows using their four-digit codes.
- Collects the numeric blocks following the CHSA rows.
- Determines whether an annual Total block precedes the monthly blocks.
- Recovers the twelve monthly values.
- Associates each monthly value with its CHSA.
- Removes duplicate year/CHSA/month combinations.
- Sorts the complete dataset into a stable order.

The parser also attempts to detect report revisions where the annual total and monthly columns appear in different positions.

## Error handling

Individual years are processed independently.

If a report cannot be downloaded, does not return a successful HTTP status, is not a valid-looking PDF, or cannot be parsed, that year is skipped and reported at the end of the run.

For example:

```bash
== 2018 ==
  downloading ...
  extracted 1236 records

== 2019 ==
  skipped 2019: HTTP 404 Not Found

Successful years: [2018, 2020]
Unavailable/failed years: [2019]
```

The program exits successfully as long as at least one report produces records. If no reports can be parsed, it exits with an error.

## Project structure

Use the [`tree`](https://linux.die.net/man/1/tree) command to view the project structure.

The cached PDFs are source artifacts and can be deleted and regenerated at any time.

## Dependencies

See the `Cargo.toml` for the depedendency list.

The generated records are sorted by:

1. year
2. CHSA code
3. month

This provides stable output ordering across runs.

For reproducible processing of a fixed dataset, keep the cached PDFs and specify an explicit year range rather than relying on the current-year default:

## Limitations

- The parser depends on the structure of the BC PDF reports. A substantial change to the report layout may require parser updates.
- A failed or unavailable report is skipped rather than preventing all other years from being processed.
- The application currently relies on the report filename convention `deaths-by-chsa-YYYY.pdf`.
- PDF text extraction can be sensitive to changes in the source document's internal structure.
- The generated `wide` total is calculated from the twelve extracted monthly values rather than copied from the PDF's annual total column.
