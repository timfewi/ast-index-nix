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
