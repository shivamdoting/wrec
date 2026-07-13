use std::{
    env,
    fmt::Write as _,
    process::{Command, ExitCode, ExitStatus},
};

#[derive(Debug, Default)]
struct Suite {
    name: String,
    passed: usize,
    failed: usize,
    ignored: usize,
    measured: usize,
    filtered: usize,
    duration: String,
}

#[derive(Debug)]
struct TestCase {
    suite: String,
    name: String,
    status: String,
}

fn main() -> ExitCode {
    match env::args().nth(1).as_deref() {
        Some("dev") => dev(),
        Some("testf") => testf(),
        _ => {
            eprintln!("usage: cargo dev | cargo testf");
            ExitCode::FAILURE
        }
    }
}

fn dev() -> ExitCode {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let daemon = Command::new(&cargo)
        .args(["build", "-p", "daemon", "--bin", "daemon"])
        .status();

    if !command_succeeded(daemon, "build the daemon") {
        return ExitCode::FAILURE;
    }

    let app = Command::new(cargo)
        .args(["run", "-p", "app"])
        .args(env::args_os().skip(2))
        .status();

    if command_succeeded(app, "run the app") {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn command_succeeded(status: std::io::Result<ExitStatus>, action: &str) -> bool {
    match status {
        Ok(status) => status.success(),
        Err(err) => {
            eprintln!("failed to {action}: {err}");
            false
        }
    }
}

fn testf() -> ExitCode {
    let output = match Command::new("sh")
        .args([
            "-c",
            "cargo test --workspace --all-targets -- --test-threads=1 2>&1",
        ])
        .output()
    {
        Ok(output) => output,
        Err(err) => {
            eprintln!("failed to run cargo test: {err}");
            return ExitCode::FAILURE;
        }
    };

    let combined = String::from_utf8_lossy(&output.stdout);
    let report = parse_report(&combined);

    if report.suites.is_empty() {
        print!("{combined}");
    } else {
        print!("{}", render_report(&report));
    }

    if output.status.success() {
        ExitCode::SUCCESS
    } else {
        if !combined.trim().is_empty() {
            println!();
            println!("raw cargo output");
            println!("{}", "-".repeat(80));
            print!("{combined}");
        }
        ExitCode::FAILURE
    }
}

#[derive(Debug, Default)]
struct Report {
    suites: Vec<Suite>,
    tests: Vec<TestCase>,
}

fn parse_report(output: &str) -> Report {
    let mut report = Report::default();
    let mut current_suite: Option<String> = None;

    for line in output.lines() {
        if let Some(name) = parse_suite_name(line) {
            current_suite = Some(name.clone());
            report.suites.push(Suite {
                name,
                ..Suite::default()
            });
            continue;
        }

        if let Some((name, status)) = parse_test_case(line) {
            report.tests.push(TestCase {
                suite: current_suite
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                name,
                status,
            });
            continue;
        }

        if let Some(summary) = parse_suite_summary(line) {
            if let Some(suite) = report.suites.last_mut() {
                suite.passed = summary.passed;
                suite.failed = summary.failed;
                suite.ignored = summary.ignored;
                suite.measured = summary.measured;
                suite.filtered = summary.filtered;
                suite.duration = summary.duration;
            }
        }
    }

    report
}

fn parse_suite_name(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let rest = trimmed
        .strip_prefix("Running unittests ")
        .or_else(|| trimmed.strip_prefix("Running tests/"))
        .or_else(|| trimmed.strip_prefix("Doc-tests "))?;

    if trimmed.starts_with("Doc-tests ") {
        return Some(format!("doc:{}", rest.trim()));
    }

    rest.split_once("(target/")
        .and_then(|(_, path)| path.rsplit('/').next())
        .and_then(|binary| binary.strip_suffix(')'))
        .map(package_from_binary)
        .or_else(|| Some(rest.trim().to_string()))
}

fn package_from_binary(binary: &str) -> String {
    binary
        .rsplit_once('-')
        .map(|(name, _)| name)
        .unwrap_or(binary)
        .replace('_', "-")
}

fn parse_test_case(line: &str) -> Option<(String, String)> {
    let rest = line.trim_start().strip_prefix("test ")?;
    let (name, status) = rest.rsplit_once(" ... ")?;
    Some((name.to_string(), status.to_string()))
}

#[derive(Debug)]
struct SuiteSummary {
    passed: usize,
    failed: usize,
    ignored: usize,
    measured: usize,
    filtered: usize,
    duration: String,
}

fn parse_suite_summary(line: &str) -> Option<SuiteSummary> {
    let rest = line.trim_start().strip_prefix("test result: ")?;
    let (status, rest) = rest.split_once(". ")?;
    let _ = status;

    Some(SuiteSummary {
        passed: parse_count(rest, "passed")?,
        failed: parse_count(rest, "failed")?,
        ignored: parse_count(rest, "ignored")?,
        measured: parse_count(rest, "measured")?,
        filtered: parse_count(rest, "filtered out")?,
        duration: rest
            .rsplit_once("finished in ")
            .map(|(_, duration)| duration.to_string())
            .unwrap_or_else(|| "-".to_string()),
    })
}

fn parse_count(summary: &str, label: &str) -> Option<usize> {
    summary
        .split(';')
        .find_map(|part| part.trim().strip_suffix(label))
        .and_then(|part| part.trim().parse().ok())
}

fn render_report(report: &Report) -> String {
    let suites = suite_table(report);
    let tests = test_table(report);
    let width = table_width(&suites.widths()).max(table_width(&tests.widths()));
    let passed = report
        .suites
        .iter()
        .map(|suite| suite.passed)
        .sum::<usize>();
    let failed = report
        .suites
        .iter()
        .map(|suite| suite.failed)
        .sum::<usize>();
    let ignored = report
        .suites
        .iter()
        .map(|suite| suite.ignored)
        .sum::<usize>();

    let mut out = String::new();
    writeln!(out, "test suites").unwrap();
    out.push_str(&render_table(&suites, width));
    writeln!(out).unwrap();
    writeln!(out, "test cases").unwrap();
    out.push_str(&render_table(&tests, width));
    writeln!(out).unwrap();
    writeln!(
        out,
        "summary: {passed} passed, {failed} failed, {ignored} ignored"
    )
    .unwrap();
    out
}

#[derive(Debug)]
struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Table {
    fn widths(&self) -> Vec<usize> {
        let mut widths = self
            .headers
            .iter()
            .map(|header| header.len())
            .collect::<Vec<_>>();
        for row in &self.rows {
            for (index, cell) in row.iter().enumerate() {
                widths[index] = widths[index].max(cell.len());
            }
        }
        widths
    }
}

fn suite_table(report: &Report) -> Table {
    Table {
        headers: strings(&[
            "suite", "passed", "failed", "ignored", "filtered", "time", "status",
        ]),
        rows: report
            .suites
            .iter()
            .map(|suite| {
                vec![
                    suite.name.clone(),
                    suite.passed.to_string(),
                    suite.failed.to_string(),
                    suite.ignored.to_string(),
                    suite.filtered.to_string(),
                    suite.duration.clone(),
                    suite_status(suite).to_string(),
                ]
            })
            .collect(),
    }
}

fn suite_status(suite: &Suite) -> &'static str {
    if suite.failed == 0 {
        "ok"
    } else {
        "failed"
    }
}

fn test_table(report: &Report) -> Table {
    Table {
        headers: strings(&["suite", "status", "test"]),
        rows: report
            .tests
            .iter()
            .map(|test| vec![test.suite.clone(), test.status.clone(), test.name.clone()])
            .collect(),
    }
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| item.to_string()).collect()
}

fn render_table(table: &Table, target_width: usize) -> String {
    let widths = padded_widths(table.widths(), target_width);
    let mut out = String::new();
    render_border(&mut out, &widths);
    render_row(&mut out, &widths, &table.headers);
    render_border(&mut out, &widths);
    for row in &table.rows {
        render_row(&mut out, &widths, row);
    }
    render_border(&mut out, &widths);
    out
}

fn padded_widths(mut widths: Vec<usize>, target_width: usize) -> Vec<usize> {
    let width = table_width(&widths);
    if let Some(last) = widths.last_mut() {
        *last += target_width.saturating_sub(width);
    }
    widths
}

fn table_width(widths: &[usize]) -> usize {
    widths.iter().sum::<usize>() + widths.len() * 3 + 1
}

fn render_border(out: &mut String, widths: &[usize]) {
    out.push('+');
    for width in widths {
        write!(out, "{}+", "-".repeat(width + 2)).unwrap();
    }
    out.push('\n');
}

fn render_row(out: &mut String, widths: &[usize], row: &[String]) {
    out.push('|');
    for (width, cell) in widths.iter().zip(row) {
        write!(out, " {cell:<width$} |").unwrap();
    }
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_OUTPUT: &str = r#"
     Running unittests src/main.rs (target/debug/deps/wrec-a1b2c3)

running 2 tests
test args::tests::help_and_version_flags ... ok
test args::tests::record_rejects_bad_values ... FAILED

test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

     Running unittests src/lib.rs (target/debug/deps/domain-d4e5f6)

running 1 test
test tests::codec_args_match_capture_engine_contract ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
"#;

    #[test]
    fn parses_cargo_test_output_into_real_suites_and_cases() {
        let report = parse_report(SAMPLE_OUTPUT);

        assert_eq!(report.suites.len(), 2);
        assert_eq!(report.suites[0].name, "wrec");
        assert_eq!(report.suites[0].passed, 1);
        assert_eq!(report.suites[0].failed, 1);
        assert_eq!(report.suites[1].name, "domain");
        assert_eq!(report.suites[1].passed, 1);

        assert_eq!(report.tests.len(), 3);
        assert_eq!(report.tests[0].suite, "wrec");
        assert_eq!(report.tests[0].status, "ok");
        assert_eq!(report.tests[1].suite, "wrec");
        assert_eq!(report.tests[1].status, "FAILED");
        assert_eq!(report.tests[2].suite, "domain");
    }

    #[test]
    fn renders_suite_and_case_tables_at_one_width() {
        let report = parse_report(SAMPLE_OUTPUT);
        let rendered = render_report(&report);
        let border_lengths = rendered
            .lines()
            .filter(|line| line.starts_with('+'))
            .map(str::len)
            .collect::<Vec<_>>();

        assert!(!border_lengths.is_empty());
        assert!(
            border_lengths
                .iter()
                .all(|length| *length == border_lengths[0]),
            "{rendered}"
        );
    }
}
