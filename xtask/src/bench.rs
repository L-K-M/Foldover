//! `bench-check`: run the criterion benches with a short measurement
//! window and hold their medians against `perf/budgets.toml`
//! (PLAN.md §5.2). Fails when any median is missing or over budget.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use crate::{ToolError, cargo, workspace_root};

/// The bench target per hot path, in the crate that owns the code.
/// Criterion ids are the bench function names, so `target/criterion/
/// <id>/new/estimates.json` is keyed by the same ids as the budgets.
const BENCHES: [(&str, &str); 4] = [
    ("foldover-fs", "hash"),
    ("foldover-endpoint", "scan"),
    ("foldover-engine", "plan"),
    ("foldover-state", "db"),
];

/// A short window keeps the gate fast; the budgets carry ~3x headroom
/// over local medians (see perf/README.md), so the extra noise is
/// priced in.
const CRITERION_FLAGS: [&str; 5] = ["--", "--warm-up-time", "1", "--measurement-time", "3"];

const BUDGETS_PATH: &str = "perf/budgets.toml";

pub fn bench_check() -> bool {
    match check() {
        Ok(()) => true,
        Err(error) => {
            eprintln!("bench-check: {error}");
            false
        }
    }
}

fn check() -> Result<(), ToolError> {
    let root = workspace_root();
    let budgets = read_budgets(&root)?;
    for (package, bench) in BENCHES {
        run_bench(&root, package, bench)?;
    }
    // The table prints before the verdict, so an over-budget run still
    // shows every median.
    let mut failures = Vec::new();
    let width = budgets
        .keys()
        .map(String::len)
        .max()
        .unwrap_or(0)
        .max("benchmark".len());
    let mut table = format!(
        "{:<width$}  {:>10}  {:>9}  status\n",
        "benchmark", "median", "budget"
    );
    for (id, budget_ms) in &budgets {
        let estimates = root.join(format!("target/criterion/{id}/new/estimates.json"));
        let median = std::fs::read_to_string(&estimates)
            .ok()
            .and_then(|content| median_ms(&content));
        let Some(median) = median else {
            table.push_str(&format!(
                "{id:<width$}  {:>10}  {budget_ms:>7} ms  missing\n",
                "-"
            ));
            failures.push(format!(
                "{id}: no criterion estimates at {}",
                estimates.display()
            ));
            continue;
        };
        let status = if median <= *budget_ms as f64 {
            "ok"
        } else {
            "over"
        };
        if status == "over" {
            failures.push(format!(
                "{id}: median {median:.1} ms exceeds the {budget_ms} ms budget"
            ));
        }
        table.push_str(&format!(
            "{id:<width$}  {:>7.1} ms  {budget_ms:>7} ms  {status}\n",
            median
        ));
    }
    print!("{table}");
    if failures.is_empty() {
        Ok(())
    } else {
        Err(ToolError(failures.join("; ")))
    }
}

/// `budgets = <id = max-median-ms>` from `perf/budgets.toml`, ordered
/// by id for the report.
fn read_budgets(root: &Path) -> Result<BTreeMap<String, u64>, ToolError> {
    let content = std::fs::read_to_string(root.join(BUDGETS_PATH))
        .map_err(|error| ToolError(format!("{BUDGETS_PATH}: {error}")))?;
    parse_budgets(&content)
}

fn parse_budgets(content: &str) -> Result<BTreeMap<String, u64>, ToolError> {
    let table: BTreeMap<String, toml::Value> =
        toml::from_str(content).map_err(|error| ToolError(format!("{BUDGETS_PATH}: {error}")))?;
    let mut budgets = BTreeMap::new();
    for (id, budget) in table {
        let Some(ms) = budget.as_integer().filter(|ms| *ms > 0) else {
            return Err(ToolError(format!(
                "{BUDGETS_PATH}: {id} must be a positive integer milliseconds value"
            )));
        };
        budgets.insert(id, ms as u64);
    }
    if budgets.is_empty() {
        return Err(ToolError(format!("{BUDGETS_PATH}: lists no budgets")));
    }
    Ok(budgets)
}

/// The median point estimate in milliseconds from a criterion
/// `estimates.json` document (criterion stores nanoseconds).
fn median_ms(content: &str) -> Option<f64> {
    let value: serde_json::Value = serde_json::from_str(content).ok()?;
    Some(value.get("median")?.get("point_estimate")?.as_f64()? / 1_000_000.0)
}

/// `cargo bench` for one explicit bench target. Naming `--bench` keeps
/// cargo from also running the crate's unit tests, whose libtest
/// harness rejects criterion's flags.
fn run_bench(root: &Path, package: &str, bench: &str) -> Result<(), ToolError> {
    let mut cmd = Command::new(cargo());
    cmd.current_dir(root)
        .args(["bench", "--locked", "-p", package, "--bench", bench])
        .args(CRITERION_FLAGS);
    eprintln!(
        "+ cargo bench --locked -p {package} --bench {bench} {}",
        CRITERION_FLAGS.join(" ")
    );
    match cmd.status() {
        Ok(status) if status.success() => Ok(()),
        Ok(_) => Err(ToolError(format!(
            "cargo bench -p {package} --bench {bench} failed"
        ))),
        Err(error) => Err(ToolError(format!(
            "cargo bench -p {package} --bench {bench}: {error}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_median_point_estimates_in_milliseconds() {
        let content = r#"{"median": {"confidence_interval": {"confidence_level": 0.95,
            "lower_bound": 1, "upper_bound": 3}, "point_estimate": 2500000}}"#;
        assert_eq!(median_ms(content), Some(2.5));
        assert_eq!(median_ms("{}"), None);
        assert_eq!(median_ms("not json"), None);
    }

    #[test]
    fn reads_positive_integer_budgets_ordered_by_id() {
        let content = "plan_100k_entries = 350\nscan_10k_files = 2000\n";
        let budgets = parse_budgets(content).unwrap();
        assert_eq!(
            budgets.into_iter().collect::<Vec<_>>(),
            [
                ("plan_100k_entries".to_owned(), 350),
                ("scan_10k_files".to_owned(), 2000)
            ]
        );
    }

    #[test]
    fn rejects_empty_or_non_integer_budgets() {
        assert!(parse_budgets("").is_err());
        assert!(parse_budgets("hash_64_mib = 60.5").is_err());
        assert!(parse_budgets("hash_64_mib = 0").is_err());
    }
}
