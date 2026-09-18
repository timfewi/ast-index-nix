//! Compact renderers for CLI and MCP output. Agent-facing output is capped so a
//! single call cannot flood the model context.

use crate::index::IndexStats;
use crate::model::{Status, Symbol};
use crate::store::{CallEdge, ImpactRow};

/// Maximum rendered lines for list-shaped output.
pub const MAX_LIST_LINES: usize = 60;

/// Render `status` as text.
pub fn status_text(status: &Status) -> String {
    if !status.indexed {
        return "not indexed".to_string();
    }
    let mut output = String::new();
    output.push_str(&format!("root: {}\n", status.root));
    output.push_str(&format!(
        "files: {}  symbols: {}  references: {}  resolved: {}  imports: {}\n",
        status.files, status.symbols, status.references, status.resolved_references, status.imports
    ));
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
    output.push_str(&format!(
        "symbols: {}  references: {}  resolved: {} (exact {}, high {}, low {})  ambiguous: {}\n",
        stats.symbols,
        stats.references,
        stats.resolution.resolved,
        stats.resolution.exact,
        stats.resolution.high,
        stats.resolution.low,
        stats.resolution.ambiguous
    ));
    for failure in &stats.failures {
        output.push_str(&format!("  warning: {failure}\n"));
    }
    output.trim_end().to_string()
}

/// Render a list of symbols as compact text.
pub fn symbols_text(symbols: &[Symbol]) -> String {
    if symbols.is_empty() {
        return "no matching symbols".to_string();
    }
    let mut output = String::new();
    for symbol in symbols.iter().take(MAX_LIST_LINES) {
        output.push_str(&format!(
            "{}:{}  {}  {}\n",
            symbol.file,
            symbol.start_line,
            symbol.kind.as_str(),
            symbol.qualified
        ));
    }
    if symbols.len() > MAX_LIST_LINES {
        output.push_str(&format!("... {} more\n", symbols.len() - MAX_LIST_LINES));
    }
    output.trim_end().to_string()
}

/// Render call edges as compact text.
pub fn edges_text(header: &str, edges: &[CallEdge]) -> String {
    if edges.is_empty() {
        return format!("{header}: none");
    }
    let mut output = format!("{header} ({}):\n", edges.len());
    for edge in edges.iter().take(MAX_LIST_LINES) {
        let caller = edge.caller.as_deref().unwrap_or("<top-level>");
        output.push_str(&format!(
            "  {} -> {}  ({}:{}, {})\n",
            caller,
            edge.callee,
            edge.file,
            edge.line,
            edge.confidence.as_str()
        ));
    }
    if edges.len() > MAX_LIST_LINES {
        output.push_str(&format!("... {} more\n", edges.len() - MAX_LIST_LINES));
    }
    output.trim_end().to_string()
}

/// Render an impact list as compact text.
pub fn impact_text(symbol: &Symbol, rows: &[ImpactRow]) -> String {
    if rows.is_empty() {
        return format!("impact of {}: no resolved callers", symbol.qualified);
    }
    let mut output = format!("impact of {} ({}):\n", symbol.qualified, rows.len());
    for row in rows.iter().take(MAX_LIST_LINES) {
        output.push_str(&format!(
            "  depth {}  {}:{}  {}\n",
            row.depth, row.symbol.file, row.symbol.start_line, row.symbol.qualified
        ));
    }
    if rows.len() > MAX_LIST_LINES {
        output.push_str(&format!("... {} more\n", rows.len() - MAX_LIST_LINES));
    }
    output.trim_end().to_string()
}
