//! Bounded SQL projections. User data is always bound, never SQL source.

use super::{Store, Symbol, symbol_from_row};
use crate::{
    error::{Error, Result},
    query::{Query, Window},
};
use rusqlite::{params_from_iter, types::Value};

struct Filter {
    sql: Vec<String>,
    values: Vec<Value>,
}

impl Filter {
    fn new(query: &Query, kind_column: &str, file_filter: bool) -> Result<Self> {
        let mut filter = Self {
            sql: Vec::new(),
            values: Vec::new(),
        };
        if file_filter && let Some(file) = &query.file {
            filter.add("f.path =", crate::query::relative_path(file)?);
        }
        if let Some(subtree) = &query.subtree {
            let path = crate::query::relative_path(subtree)?;
            if !path.is_empty() {
                let p = filter.bind(path.clone());
                let prefix = filter.bind(format!("{}/%", escape_like(&path)));
                filter.sql.push(format!(
                    "(f.path = {p} OR f.path LIKE {prefix} ESCAPE '\\')"
                ));
            }
        }
        if let Some(language) = &query.language {
            if language == "tsx" {
                filter.add("f.lang =", "typescript".to_string());
                filter.sql.push("lower(f.path) LIKE '%.tsx'".into());
            } else {
                filter.add("f.lang =", language.clone());
            }
        }
        if let Some(kind) = &query.kind {
            filter.add(&format!("{kind_column} ="), kind.clone());
        }
        Ok(filter)
    }
    fn bind(&mut self, value: impl Into<Value>) -> String {
        self.values.push(value.into());
        format!("?{}", self.values.len())
    }
    fn add(&mut self, operator: &str, value: impl Into<Value>) {
        let param = self.bind(value);
        self.sql.push(format!("{operator} {param}"));
    }
    fn clause(&self) -> String {
        if self.sql.is_empty() {
            "1".into()
        } else {
            self.sql.join(" AND ")
        }
    }
    fn bounds(&mut self, window: &Window) -> String {
        let limit = self.bind((window.limit + 1) as i64);
        let offset = self.bind(window.offset as i64);
        format!("LIMIT {limit} OFFSET {offset}")
    }
}

pub(crate) fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn symbol_projection(query: &Query, window: &Window, exact: bool) -> Result<(String, Vec<Value>)> {
    let mut filter = Filter::new(query, "s.kind", true)?;
    if let Some(name) = &query.query {
        let mode = if exact {
            "exact"
        } else {
            query.match_mode.as_deref().unwrap_or("contains")
        };
        let lower = if exact {
            name.clone()
        } else {
            name.to_lowercase()
        };
        let pattern = match mode {
            "prefix" => format!("{}%", escape_like(&lower)),
            "contains" => format!("%{}%", escape_like(&lower)),
            _ => lower,
        };
        let parameter = filter.bind(pattern);
        let (column, qualified) = if exact {
            ("s.name", "s.qualified")
        } else {
            ("s.name_lower", "s.qualified_lower")
        };
        filter.sql.push(if mode == "exact" { format!("({column} = {parameter} OR {qualified} = {parameter})") }
                else { format!("({column} LIKE {parameter} ESCAPE '\\' OR {qualified} LIKE {parameter} ESCAPE '\\')") });
    }
    if let Some(line) = query.line {
        let p = filter.bind(i64::from(line));
        filter
            .sql
            .push(format!("s.start_line <= {p} AND s.end_line >= {p}"));
        if let Some(column) = query.column {
            let c = filter.bind(i64::from(column));
            filter.sql.push(format!("(s.start_line < {p} OR s.start_column <= {c}) AND (s.end_line > {p} OR s.end_column > {c})"));
        }
    }
    let bounds = filter.bounds(window);
    let sql = format!("SELECT s.id, f.path, s.name, s.kind, s.start_line, s.end_line, s.parent, s.qualified, s.start_column, s.end_column
            FROM symbols s JOIN files f ON f.id = s.file_id WHERE {} ORDER BY f.path, s.start_line, s.id {bounds}", filter.clause());
    Ok((sql, filter.values))
}

impl Store {
    pub(crate) fn select_symbols(
        &self,
        query: &Query,
        window: &Window,
        exact: bool,
    ) -> Result<Vec<Symbol>> {
        let (sql, values) = symbol_projection(query, window, exact)?;
        let mut statement = self.conn.prepare(&sql)?;
        Ok(statement
            .query_map(params_from_iter(values), symbol_from_row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub(crate) fn select_edges(
        &self,
        query: &Query,
        window: &Window,
        id: i64,
        incoming: bool,
    ) -> Result<Vec<super::CallEdge>> {
        let mut filter = Filter::new(query, "s.kind", false)?;
        filter.sql.push("r.kind = 'call'".into());
        filter.add(
            if incoming {
                "r.resolved_symbol_id ="
            } else {
                "r.from_symbol_id ="
            },
            id,
        );
        let bounds = filter.bounds(window);
        let neighbor = if incoming {
            "r.from_symbol_id"
        } else {
            "r.resolved_symbol_id"
        };
        let sql = format!(
            "SELECT caller.qualified, callee.qualified, site.path, r.line, r.confidence, r.reason
            FROM refs r JOIN files site ON site.id = r.file_id
            LEFT JOIN symbols caller ON caller.id = r.from_symbol_id
            JOIN symbols callee ON callee.id = r.resolved_symbol_id
            LEFT JOIN symbols s ON s.id = {neighbor}
            LEFT JOIN files f ON f.id = COALESCE(s.file_id, r.file_id)
            WHERE {} ORDER BY site.path, r.line, r.id {bounds}",
            filter.clause()
        );
        let mut statement = self.conn.prepare(&sql)?;
        Ok(statement
            .query_map(params_from_iter(filter.values), |r| {
                Ok(super::CallEdge {
                    caller: r.get(0)?,
                    callee: r.get(1)?,
                    file: r.get(2)?,
                    line: r.get::<_, u32>(3)?,
                    confidence: super::parse_confidence(r.get(4)?),
                    reason: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub(crate) fn select_refs(
        &self,
        query: &Query,
        window: &Window,
    ) -> Result<Vec<crate::model::Reference>> {
        let mut filter = Filter::new(query, "r.kind", true)?;
        if let Some(reason) = &query.reason {
            filter.add("r.reason =", reason.clone());
        }
        if let Some(name) = &query.query {
            filter.add("r.name =", name.clone());
        }
        let bounds = filter.bounds(window);
        let sql = format!("SELECT r.id, f.path, r.name, r.kind, r.line, s.qualified,
            r.resolved_symbol_id, r.confidence, r.path, r.reason
            FROM refs r JOIN files f ON f.id = r.file_id LEFT JOIN symbols s ON s.id = r.from_symbol_id
            WHERE {} ORDER BY f.path, r.line, r.id {bounds}", filter.clause());
        let mut statement = self.conn.prepare(&sql)?;
        Ok(statement
            .query_map(params_from_iter(filter.values), |r| {
                Ok(crate::model::Reference {
                    id: r.get(0)?,
                    file: r.get(1)?,
                    name: r.get(2)?,
                    kind: crate::model::RefKind::from_stored(&r.get::<_, String>(3)?)
                        .unwrap_or(crate::model::RefKind::Call),
                    line: r.get(4)?,
                    from_symbol: r.get(5)?,
                    resolved: r.get(6)?,
                    confidence: super::parse_confidence(r.get(7)?),
                    path: r.get(8)?,
                    reason: r.get(9)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub(crate) fn select_impact(
        &self,
        query: &Query,
        window: &Window,
        id: i64,
    ) -> Result<Vec<super::ImpactRow>> {
        let ids = self.impact_ids(id, query.depth.unwrap_or(3))?;
        let mut filter = Filter::new(query, "s.kind", false)?;
        let reachable = filter.bind(serde_json::to_string(&ids)?);
        let bounds = filter.bounds(window);
        let sql = format!("SELECT s.id, f.path, s.name, s.kind, s.start_line, s.end_line, s.parent, s.qualified, s.start_column, s.end_column,
            json_extract(reach.value, '$[1]') AS depth
            FROM json_each({reachable}) reach JOIN symbols s ON s.id = json_extract(reach.value, '$[0]')
            JOIN files f ON f.id = s.file_id WHERE {} ORDER BY depth, f.path, s.start_line, s.id {bounds}", filter.clause());
        let mut statement = self.conn.prepare(&sql)?;
        Ok(statement
            .query_map(params_from_iter(filter.values), |r| {
                Ok(super::ImpactRow {
                    symbol: symbol_from_row(r)?,
                    depth: r.get(10)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub(crate) fn select_imports(
        &self,
        query: &Query,
        window: &Window,
    ) -> Result<Vec<crate::imports::Import>> {
        let incoming = query.action == "importers";
        let mut filter = Filter::new(query, "r.kind", !incoming)?;
        filter.sql.push("r.kind = 'import'".into());
        if incoming {
            let file = query
                .file
                .as_deref()
                .ok_or_else(|| Error::Invalid("importers requires file".into()))?;
            filter.add("r.import_target =", crate::query::relative_path(file)?);
        }
        if query.action == "deps" {
            filter.sql.push("r.import_target IS NOT NULL".into());
        }
        if let Some(reason) = &query.reason {
            filter.add("r.reason =", reason.clone());
        }
        let bounds = filter.bounds(window);
        let sql = format!("SELECT f.path, r.line, r.name, r.import_target, r.reason FROM refs r JOIN files f ON f.id = r.file_id
            WHERE {} ORDER BY f.path, r.line, r.id {bounds}", filter.clause());
        let mut statement = self.conn.prepare(&sql)?;
        Ok(statement
            .query_map(params_from_iter(filter.values), |r| {
                Ok(crate::imports::Import {
                    file: r.get(0)?,
                    line: r.get(1)?,
                    name: r.get(2)?,
                    target: r.get(3)?,
                    reason: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub(crate) fn select_one(&self, query: &Query) -> Result<Symbol> {
        let mut selector = Query {
            action: query.action.clone(),
            query: query.query.clone(),
            file: query.file.clone(),
            line: query.line,
            column: query.column,
            ..Query::default()
        };
        selector.cursor = None;
        selector.limit = Some(25);
        let window = selector.window(self.generation()?)?;
        let matches = self.select_symbols(&selector, &window, true)?;
        match matches.len() {
            0 => Err(Error::Invalid(
                "no matching symbol; use search and select file, query and optionally line".into(),
            )),
            1 => Ok(matches.into_iter().next().expect("one symbol")),
            _ => Err(Error::Ambiguous(matches)),
        }
    }
}

impl Store {
    pub(crate) fn repository_map(
        &self,
        query: &Query,
        window: &Window,
    ) -> Result<serde_json::Value> {
        let budget = query.budget.unwrap_or(8192);
        if !(1024..=32768).contains(&budget) {
            return Err(Error::Invalid(
                "map budget must be 1024..32768 bytes".into(),
            ));
        }
        if query.cursor.is_some() {
            return Err(Error::Invalid(
                "repository maps use a budget, not pagination".into(),
            ));
        }
        let mut filter = Filter::new(query, "s.kind", true)?;
        let focus = filter.bind(query.query.clone().unwrap_or_default().to_lowercase());
        let sql = format!("SELECT s.id,f.path,s.name,s.kind,s.start_line,s.end_line,s.parent,s.qualified,s.start_column,s.end_column,s.signature,
            (SELECT COUNT(*) FROM refs r WHERE r.resolved_symbol_id=s.id AND r.kind='call') +
            CASE WHEN {focus}!='' AND (s.name_lower={focus} OR s.qualified_lower={focus}) THEN 1000 ELSE 0 END +
            CASE WHEN EXISTS(SELECT 1 FROM refs r JOIN symbols focus ON focus.id=r.from_symbol_id
                WHERE r.resolved_symbol_id=s.id AND (focus.name_lower={focus} OR focus.qualified_lower={focus})) THEN 100 ELSE 0 END AS score
            FROM symbols s JOIN files f ON f.id=s.file_id WHERE {} ORDER BY score DESC,f.path,s.start_line,s.id LIMIT 101", filter.clause());
        let mut statement = self.conn.prepare(&sql)?;
        let rows: Vec<serde_json::Value> = statement.query_map(params_from_iter(filter.values), |r| Ok(serde_json::json!({
            "symbol":symbol_from_row(r)?, "signature":r.get::<_,String>(10)?, "score":r.get::<_,i64>(11)?
        })))?.collect::<rusqlite::Result<_>>()?;
        let candidate_count = rows.len();
        let mut response = serde_json::json!({"items":[],"generation":window.generation,"truncated":false,
            "ranking":"heuristic: static incoming calls and optional symbol focus","budget_bytes":budget,
            "token_estimate":0,"token_estimate_method":"UTF-8 bytes / 4; approximate"});
        let mut items = Vec::new();
        for row in rows.into_iter().take(query.limit.unwrap_or(100)) {
            items.push(row);
            response["items"] = serde_json::json!(items);
            if serde_json::to_vec(&response)?.len() + 80 > budget {
                items.pop();
                break;
            }
        }
        response["truncated"] = serde_json::json!(items.len() < candidate_count);
        response["items"] = serde_json::json!(items);
        response["token_estimate"] =
            serde_json::json!(serde_json::to_vec(&response)?.len().div_ceil(4));
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_and_prefix_use_indexes_while_contains_is_a_bounded_scan() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("index.sqlite")).unwrap();
        let file = store.upsert_file("lib.rs", "rust", 0, 0, "").unwrap();
        let symbols: Vec<_> = (0..1000)
            .map(|i| super::super::SymbolInsert {
                name: format!("symbol_{i:04}"),
                qualified: format!("module::symbol_{i:04}"),
                kind: crate::model::SymbolKind::Function,
                start_line: i + 1,
                end_line: i + 1,
                start_column: 1,
                end_column: 10,
                parent: None,
            })
            .collect();
        store.insert_symbols(file, &symbols).unwrap();
        store.conn.execute_batch("ANALYZE").unwrap();
        for mode in ["exact", "prefix", "contains"] {
            let query = Query {
                action: "search".into(),
                query: Some("symbol_001".into()),
                match_mode: Some(mode.into()),
                ..Query::default()
            };
            let window = query.window(0).unwrap();
            let (sql, values) = symbol_projection(&query, &window, false).unwrap();
            let plan: Vec<String> = store
                .conn
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap()
                .query_map(params_from_iter(values), |r| r.get(3))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            eprintln!("{mode}: {}", plan.join("; "));
            if mode != "contains" {
                assert!(
                    plan.iter().any(|s| s.contains("INDEX idx_symbols")),
                    "{plan:?}"
                );
            } else {
                assert!(plan.iter().any(|s| s.contains("SCAN s")), "{plan:?}");
            }
        }
    }
}
