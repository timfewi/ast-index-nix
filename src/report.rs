//! Compact renderers for CLI and MCP output. Agent-facing output is capped so a
//! single call cannot flood the model context.

use crate::index::IndexStats;
use crate::model::Status;

/// Render `status` as text.
pub fn status_text(status: &Status) -> String {
    if !status.indexed {
        return "not indexed".to_string();
    }
    let mut output = String::new();
    output.push_str(&format!(
        "root: {}\nfreshness: {}  generation: {}  last indexed: {:?}\n",
        status.root, status.freshness, status.generation, status.last_indexed_at
    ));
    output.push_str(&format!(
        "files: {}  symbols: {}  references: {}  resolved: {}  imports: {}\n",
        status.files, status.symbols, status.references, status.resolved_references, status.imports
    ));
    if let Some(health) = &status.health {
        output.push_str(&format!(
            "scan complete: {}  failed: {}  skipped: {}  parse-error files: {}\n",
            health.scan_complete,
            health.files_failed,
            health.files_skipped,
            health.parse_error_files
        ));
    }
    for (reason, count) in &status.resolution_reasons {
        output.push_str(&format!("  references {reason}: {count}\n"));
    }
    for language in &status.languages {
        output.push_str(&format!(
            "  {:<12} files {:>5}  symbols {}\n",
            language.language, language.files, language.symbols
        ));
    }
    output.trim_end().to_string()
}

/// Render `status` for people: plain-language state, readable timestamps,
/// grouped thousands and sorted breakdowns. `now` is Unix seconds.
pub fn status_nice(status: &Status, now: i64) -> String {
    if !status.indexed {
        return "Not indexed yet. Run `ast-index index` in the project root.".to_string();
    }
    let mut out = format!("ast-index  {}\n\n", status.root);
    let state = match status.freshness.as_str() {
        "incomplete" => "incomplete: the last index run or check had incomplete results",
        "stale" => "stale: index is out of date, run `ast-index index`",
        "unknown" => "no known problems; `ast-index freshness` checks current files",
        other => other,
    };
    out.push_str(&format!("  State         {state}\n"));
    if let Some(health) = &status.health {
        if !health.scan_complete {
            out.push_str("                - scan stopped early or left files out\n");
        }
        if health.files_failed > 0 {
            out.push_str(&format!(
                "                - {} files failed\n",
                group(health.files_failed as i64)
            ));
        }
        if health.files_skipped > 0 {
            out.push_str(&format!(
                "                - {} files skipped\n",
                group(health.files_skipped as i64)
            ));
        }
        if health.parse_error_files > 0 {
            out.push_str(&format!(
                "                - {} files with syntax errors\n",
                group(health.parse_error_files as i64)
            ));
        }
        for failure in &health.failures {
            out.push_str(&format!("                - {failure}\n"));
        }
    }
    if let Some(check) = &status.last_freshness_check {
        out.push_str(&format!(
            "  Last check    {}: {} ({} changed files)\n",
            when(check.checked_at, now),
            check.state,
            group(check.changed_files as i64)
        ));
    }
    out.push_str(&format!(
        "  Indexed       {}\n  Generation    {}\n\n",
        status
            .last_indexed_at
            .map_or_else(|| "never".to_string(), |at| when(at, now)),
        status.generation
    ));
    let resolved = format!(
        "{} ({} linked to a definition)",
        group(status.references),
        percent(status.resolved_references, status.references)
    );
    for (label, value) in [
        ("Files", group(status.files)),
        ("Symbols", group(status.symbols)),
        ("References", resolved),
        ("Imports", group(status.imports)),
    ] {
        out.push_str(&format!("  {label:<13} {value}\n"));
    }
    let mut languages: Vec<_> = status.languages.iter().collect();
    languages.sort_by(|a, b| {
        b.files
            .cmp(&a.files)
            .then_with(|| a.language.cmp(&b.language))
    });
    out.push_str("\n  Language          Files    Symbols\n");
    for language in languages {
        out.push_str(&format!(
            "  {:<12} {:>10} {:>10}\n",
            language.language,
            group(language.files),
            group(language.symbols)
        ));
    }
    let mut reasons: Vec<_> = status.resolution_reasons.iter().collect();
    reasons.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    let width = reasons
        .iter()
        .map(|(reason, _)| reason.len())
        .max()
        .unwrap_or(0);
    out.push_str("\n  Reference outcomes, most frequent first\n");
    for (reason, count) in reasons {
        out.push_str(&format!(
            "  {reason:<width$} {:>10} {:>6}\n",
            group(*count),
            percent(*count, status.references)
        ));
    }
    out.trim_end().to_string()
}

/// `1234567` -> `1,234,567`.
fn group(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if value < 0 {
        out.push('-');
    }
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

fn percent(part: i64, total: i64) -> String {
    if total <= 0 {
        return "-".to_string();
    }
    format!("{:.1}%", part as f64 * 100.0 / total as f64)
}

/// UTC timestamp plus a coarse age, e.g. `2026-10-23 08:40 UTC (3 h ago)`.
fn when(at: i64, now: i64) -> String {
    let (days, seconds) = (at.div_euclid(86_400), at.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (proleptic Gregorian calendar).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let age = now - at;
    let ago = match age {
        i64::MIN..=-1 => "in the future".to_string(),
        0..=59 => "just now".to_string(),
        60..=3_599 => format!("{} min ago", age / 60),
        3_600..=86_399 => format!("{} h ago", age / 3_600),
        _ => format!("{} days ago", age / 86_400),
    };
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC ({ago})",
        seconds / 3_600,
        seconds % 3_600 / 60
    )
}

/// Render one indexing summary as text.
pub fn index_text(stats: &IndexStats) -> String {
    let mut output = format!(
        "indexed {} files ({} unchanged, {} removed, {} skipped, {} failed) in {} ms\n",
        stats.files_indexed,
        stats.files_unchanged,
        stats.files_removed,
        stats.files_skipped,
        stats.files_failed,
        stats.duration_ms
    );
    if !stats.resolution_recomputed {
        output.push_str("resolution reused (no structural changes)\n");
    }
    output.push_str(&format!(
        "symbols: {}  references: {}  resolved: {} (same_file {}, same_directory {}, same_project {})  ambiguous: {}\n",
        stats.symbols,
        stats.references,
        stats.resolution.resolved,
        stats.resolution.same_file,
        stats.resolution.same_directory,
        stats.resolution.same_project,
        stats.resolution.ambiguous
    ));
    for failure in &stats.failures {
        output.push_str(&format!("  warning: {failure}\n"));
    }
    output.trim_end().to_string()
}

/// Shared compact renderer for the query contract. Structured output is identical
/// across transports; pagination remains visible in the text representation.
pub fn query_text(value: &serde_json::Value) -> String {
    if let Ok(status) = serde_json::from_value::<Status>(value.clone()) {
        return status_text(&status);
    }
    if let Some(items) = value.get("items").and_then(serde_json::Value::as_array) {
        let mut lines = Vec::new();
        for key in [
            "ranking",
            "coverage",
            "evidence",
            "baseline",
            "baseline_available",
            "current_coverage_complete",
            "snapshot",
            "excluded",
            "stale_count",
            "stale_files",
            "unsupported",
            "freshness",
            "range_convention",
            "budget_bytes",
            "token_estimate",
        ] {
            if let Some(value) = value.get(key) {
                lines.push(format!(
                    "{key}: {}",
                    value
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| value.to_string())
                ));
            }
        }
        lines.extend(items.iter().map(item_text));
        if lines.is_empty() {
            lines.push("no matching results in the indexed snapshot".into());
        }
        if value["truncated"] == true {
            if let Some(cursor) = value["next_cursor"].as_str() {
                lines.push(format!("more results; next_cursor: {cursor}"));
            } else {
                lines.push("additional candidates omitted by the output budget".into());
            }
        }
        return lines.join("\n");
    }
    if value.get("symbol").is_some() && value.get("callers").is_some() {
        return format!(
            "{}\ncallers:\n{}\ncallees:\n{}",
            item_text(&value["symbol"]),
            query_text(&value["callers"]),
            query_text(&value["callees"])
        );
    }
    serde_json::to_string_pretty(value).unwrap_or_default()
}

fn item_text(value: &serde_json::Value) -> String {
    let s = |key: &str| value[key].as_str().unwrap_or_default();
    if let Some(signature) = value.get("signature").and_then(serde_json::Value::as_str) {
        return format!("{}  | {}", item_text(&value["symbol"]), signature);
    }
    if let Some(symbol) = value.get("symbol").and_then(serde_json::Value::as_str) {
        return format!(
            "{}  {}  {}  {}  (SCIP)",
            s("file"),
            value["range"],
            s("relation"),
            symbol
        );
    }
    if value.get("target").is_some() {
        return format!(
            "{}:{}  import {} -> {} ({})",
            s("file"),
            value["line"],
            s("name"),
            value["target"].as_str().unwrap_or("unresolved"),
            s("reason")
        );
    }
    if value.get("symbol").is_some() {
        return format!("depth {}  {}", value["depth"], item_text(&value["symbol"]));
    }
    if value.get("callee").is_some() {
        return format!(
            "{} -> {}  ({}:{}, {})",
            value["caller"].as_str().unwrap_or("<top-level>"),
            s("callee"),
            s("file"),
            value["line"],
            s("reason")
        );
    }
    if value.get("qualified").is_some() {
        return format!(
            "{}:{}  {}  {}",
            s("file"),
            value["start_line"],
            s("kind"),
            s("qualified")
        );
    }
    if value.get("reason").is_some() {
        return format!(
            "{}:{}  {}  {}  {}",
            s("file"),
            value["line"],
            s("kind"),
            s("name"),
            s("reason")
        );
    }
    serde_json::to_string(value).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_render_as_utc_dates_with_age() {
        assert_eq!(when(0, 30), "1970-01-01 00:00 UTC (just now)");
        // Leap day and a month boundary exercise the civil-date conversion.
        assert_eq!(
            when(951_782_400, 951_782_400 + 7_200),
            "2000-02-29 00:00 UTC (2 h ago)"
        );
        assert_eq!(
            when(1_792_783_235, 1_792_783_235 + 3 * 86_400),
            "2026-10-23 19:20 UTC (3 days ago)"
        );
        assert_eq!(when(100, 0), "1970-01-01 00:01 UTC (in the future)");
    }

    #[test]
    fn counts_group_thousands() {
        assert_eq!(group(0), "0");
        assert_eq!(group(999), "999");
        assert_eq!(group(1_000), "1,000");
        assert_eq!(group(238_753), "238,753");
        assert_eq!(group(-1_234_567), "-1,234,567");
        assert_eq!(percent(1, 0), "-");
    }

    #[test]
    fn parse_errors_alone_do_not_claim_missing_files() {
        let status: Status = serde_json::from_value(serde_json::json!({
            "root": "/r", "indexed": true, "last_indexed_at": 0, "files": 1200,
            "symbols": 3, "references": 4, "imports": 0, "resolved_references": 1,
            "languages": [], "generation": 2, "freshness": "incomplete",
            "last_freshness_check": null, "parser_versions": {},
            "current_parser_versions": {}, "resolution_version": null,
            "current_resolution_version": "v", "resolution_reasons": {},
            "health": {
                "files_seen": 1200, "files_indexed": 1200, "files_unchanged": 0,
                "files_removed": 0, "files_skipped": 0, "files_failed": 0,
                "symbols": 3, "references": 4, "duration_ms": 1,
                "resolution": crate::resolve::ResolutionStats::default(),
                "failures": [], "scan_complete": true, "parse_error_files": 2
            }
        }))
        .unwrap();
        let text = status_nice(&status, 0);
        assert!(text.contains(
            "State         incomplete: the last index run or check had incomplete results\n"
        ));
        assert!(text.contains("- 2 files with syntax errors\n"));
        assert!(!text.contains("scan stopped early"));
        assert!(text.contains("Files         1,200\n"));
        assert!(text.contains("References    4 (25.0% linked to a definition)"));
    }
}
