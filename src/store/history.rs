//! Two bounded structural snapshots. Ids are local to a snapshot; comparisons
//! use file/kind/qualified identities and report duplicate identities explicitly.
use super::{Store, symbol_from_row};
use crate::{
    error::{Error, Result},
    query::{Query, Window},
};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::{HashSet, VecDeque};

impl Store {
    pub(crate) fn save_snapshot(&self, complete: bool) -> Result<()> {
        let generation = self.generation()?;
        let symbols: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))?;
        let edges: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM refs WHERE kind = 'call' AND resolved_symbol_id IS NOT NULL",
            [],
            |r| r.get(0),
        )?;
        let within_budget = symbols <= 200_000 && edges <= 500_000;
        let note = if !within_budget {
            "snapshot size budget exceeded"
        } else if !complete {
            "incomplete index coverage"
        } else {
            "structural snapshot; heuristic call edges"
        };
        self.conn.execute(
            "INSERT INTO snapshots(generation, complete, note) VALUES(?1, ?2, ?3)",
            params![generation, complete && within_budget, note],
        )?;
        if within_budget {
            self.conn.execute("INSERT INTO snapshot_symbols(generation,symbol_id,identity,file,name,kind,start_line,end_line,parent,qualified,start_column,end_column,content_hash) SELECT ?1, s.id, json_array(f.path, s.kind, s.qualified), f.path,
                s.name, s.kind, s.start_line, s.end_line, s.parent, s.qualified, s.start_column, s.end_column, s.content_hash FROM symbols s JOIN files f ON f.id = s.file_id", [generation])?;
            self.conn.execute("INSERT INTO snapshot_edges SELECT ?1, from_symbol_id, resolved_symbol_id, f.path, r.line, r.reason
                FROM refs r JOIN files f ON f.id = r.file_id WHERE r.kind = 'call' AND r.resolved_symbol_id IS NOT NULL", [generation])?;
        }
        for table in ["snapshot_symbols", "snapshot_edges", "snapshots"] {
            self.conn.execute(&format!("DELETE FROM {table} WHERE generation NOT IN (SELECT generation FROM snapshots ORDER BY generation DESC LIMIT 2)"), [])?;
        }
        Ok(())
    }

    fn snapshot_pair(&self, baseline: Option<i64>) -> Result<(i64, i64)> {
        let current: Option<i64> =
            self.conn
                .query_row("SELECT MAX(generation) FROM snapshots", [], |r| r.get(0))?;
        let current = current.ok_or_else(|| {
            Error::Invalid("no structural snapshot available; index the root first".into())
        })?;
        let baseline = match baseline {
            Some(generation) => generation,
            None => self
                .conn
                .query_row(
                    "SELECT MAX(generation) FROM snapshots WHERE generation < ?1",
                    [current],
                    |r| r.get::<_, Option<i64>>(0),
                )?
                .ok_or_else(|| Error::Invalid("no previous structural snapshot retained".into()))?,
        };
        if baseline >= current
            || !self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM snapshots WHERE generation = ?1)",
                [baseline],
                |r| r.get::<_, bool>(0),
            )?
        {
            return Err(Error::Invalid(
                "baseline is not a retained earlier structural snapshot".into(),
            ));
        }
        Ok((current, baseline))
    }

    pub(crate) fn snapshot_diff(&self, query: &Query, window: &Window) -> Result<Value> {
        let health: Option<crate::index::IndexStats> = self
            .meta("health")?
            .map(|s| serde_json::from_str(&s))
            .transpose()?;
        if health
            .as_ref()
            .is_none_or(|h| !h.scan_complete || h.files_failed > 0 || h.parse_error_files > 0)
        {
            return Err(Error::Invalid(
                "snapshot comparison unavailable: current coverage is incomplete".into(),
            ));
        }
        let (current, baseline) = self.snapshot_pair(query.baseline)?;
        let complete: bool = self.conn.query_row(
            "SELECT MIN(complete) FROM snapshots WHERE generation IN (?1, ?2)",
            params![current, baseline],
            |r| r.get(0),
        )?;
        if !complete {
            return Err(Error::Invalid("snapshot comparison unavailable: one snapshot has incomplete coverage or exceeded its budget".into()));
        }
        let sql = "WITH old AS (SELECT *, COUNT(*) OVER (PARTITION BY identity) AS n FROM snapshot_symbols WHERE generation = ?1),
            new AS (SELECT *, COUNT(*) OVER (PARTITION BY identity) AS n FROM snapshot_symbols WHERE generation = ?2),
            symbol_changes AS (
                SELECT COALESCE(n.identity, o.identity) AS identity, COALESCE(n.file, o.file) AS file,
                    CASE WHEN COALESCE(n.n,0)>1 OR COALESCE(o.n,0)>1 THEN 'ambiguous_identity'
                    WHEN o.symbol_id IS NULL THEN 'added' WHEN n.symbol_id IS NULL THEN 'removed' ELSE 'changed' END AS change
                FROM old o FULL OUTER JOIN new n ON o.identity = n.identity
                WHERE o.symbol_id IS NULL OR n.symbol_id IS NULL OR o.content_hash != n.content_hash OR o.n>1 OR n.n>1
            ), old_edges AS (
                SELECT json_array(c.identity, t.identity, e.file) AS identity, e.file, COUNT(*) AS n
                FROM snapshot_edges e JOIN old c ON c.symbol_id=e.caller JOIN old t ON t.symbol_id=e.callee
                WHERE e.generation=?1 AND c.n=1 AND t.n=1 GROUP BY 1,2
            ), new_edges AS (
                SELECT json_array(c.identity, t.identity, e.file) AS identity, e.file, COUNT(*) AS n
                FROM snapshot_edges e JOIN new c ON c.symbol_id=e.caller JOIN new t ON t.symbol_id=e.callee
                WHERE e.generation=?2 AND c.n=1 AND t.n=1 GROUP BY 1,2
            ), changes AS (
                SELECT DISTINCT identity,file,change FROM symbol_changes UNION ALL
                SELECT COALESCE(n.identity,o.identity), COALESCE(n.file,o.file),
                    CASE WHEN o.identity IS NULL THEN 'relationship_added' WHEN n.identity IS NULL THEN 'relationship_removed' ELSE 'relationship_count_changed' END
                FROM old_edges o FULL OUTER JOIN new_edges n ON o.identity=n.identity WHERE o.identity IS NULL OR n.identity IS NULL OR o.n!=n.n
            ) SELECT identity,file,change FROM changes WHERE (?3 IS NULL OR file=?3)
                AND (?4 IS NULL OR file=?4 OR substr(file,1,length(?4)+1)=?4||'/')
                ORDER BY file,change,identity LIMIT ?5 OFFSET ?6";
        let mut statement = self.conn.prepare(sql)?;
        let rows: Vec<Value> = statement.query_map(params![baseline,current,query.file,query.subtree,(window.limit+1) as i64,window.offset as i64], |r| {
            Ok(json!({"identity":r.get::<_, String>(0)?,"file":r.get::<_, String>(1)?,"change":r.get::<_, String>(2)?}))
        })?.collect::<rusqlite::Result<_>>()?;
        let mut value = serde_json::to_value(window.page(rows)?)?;
        value["baseline"] = json!(baseline);
        value["snapshot"] = json!(current);
        value["evidence"] =
            json!("AST content and heuristic call edges; ambiguous identities are not matched");
        Ok(value)
    }

    pub(crate) fn changed_impact(&self, query: &Query, window: &Window) -> Result<Value> {
        if query.changes.is_empty() {
            return Err(Error::Invalid(
                "changes requires at least one explicit changed line range".into(),
            ));
        }
        let baseline = query.baseline.or(self
            .conn
            .query_row(
                "SELECT generation FROM snapshots ORDER BY generation DESC LIMIT 1 OFFSET 1",
                [],
                |r| r.get(0),
            )
            .optional()?);
        if let Some(generation) = baseline {
            let complete: Option<bool> = self
                .conn
                .query_row(
                    "SELECT complete FROM snapshots WHERE generation=?1",
                    [generation],
                    |r| r.get(0),
                )
                .optional()?;
            if complete != Some(true) {
                return Err(Error::Invalid(
                    "requested baseline is missing or incomplete".into(),
                ));
            }
        }
        let mut all = Vec::new();
        let mut unsupported = Vec::new();
        let mut work_left = 20_000usize;
        for (origin, generation) in [("current", None), ("baseline", baseline)] {
            if origin == "baseline" && generation.is_none() {
                continue;
            }
            let mut seeds = HashSet::new();
            for change in &query.changes {
                let path = crate::query::relative_path(&change.file)?;
                let sql = if generation.is_some() {
                    "SELECT symbol_id FROM snapshot_symbols WHERE file=?1 AND start_line<=?2 AND end_line>=?3 AND generation=?4 LIMIT 1001"
                } else {
                    "SELECT s.id FROM symbols s JOIN files f ON f.id=s.file_id WHERE f.path=?1 AND s.start_line<=?2 AND s.end_line>=?3 AND ?4 IS NULL LIMIT 1001"
                };
                let ids: Vec<i64> = self
                    .conn
                    .prepare(sql)?
                    .query_map(
                        params![path, change.end_line, change.start_line, generation],
                        |r| r.get(0),
                    )?
                    .collect::<rusqlite::Result<_>>()?;
                if ids.is_empty() && origin == "current" {
                    unsupported.push(json!({"file":path,"reason":if baseline.is_some(){"no_current_symbol; baseline also queried"}else{"no_current_symbol_and_no_baseline"}}));
                }
                seeds.extend(ids);
                if seeds.len() > 1000 {
                    return Err(Error::Invalid(
                        "changed-symbol budget exceeded; narrow line ranges".into(),
                    ));
                }
            }
            let mut sorted: Vec<_> = seeds.iter().copied().collect();
            sorted.sort();
            let mut queue: VecDeque<_> = sorted.into_iter().map(|id| (id, 0u32)).collect();
            let mut seen = seeds;
            while let Some((id, depth)) = queue.pop_front() {
                all.push((origin, depth, id));
                if all.len() > 20_000 {
                    return Err(Error::Invalid(
                        "change impact result budget exceeded; narrow changes".into(),
                    ));
                }
                if depth >= query.depth.unwrap_or(3) {
                    continue;
                }
                let sql = if generation.is_some() {
                    "SELECT DISTINCT caller FROM snapshot_edges WHERE callee=?1 AND generation=?2 AND caller IS NOT NULL ORDER BY caller LIMIT ?3"
                } else {
                    "SELECT DISTINCT from_symbol_id FROM refs WHERE resolved_symbol_id=?1 AND kind='call' AND from_symbol_id IS NOT NULL AND ?2 IS NULL ORDER BY from_symbol_id LIMIT ?3"
                };
                let ids: Vec<i64> = self
                    .conn
                    .prepare(sql)?
                    .query_map(params![id, generation, (work_left + 1) as i64], |r| {
                        r.get(0)
                    })?
                    .collect::<rusqlite::Result<_>>()?;
                if ids.len() > work_left {
                    return Err(Error::Invalid(
                        "change impact traversal budget exceeded".into(),
                    ));
                }
                work_left -= ids.len();
                for id in ids {
                    if seen.insert(id) {
                        queue.push_back((id, depth + 1));
                    }
                }
            }
        }
        let rows_json = serde_json::to_string(&all)?;
        let sql = "WITH reached AS (SELECT json_extract(value,'$[0]') AS origin, json_extract(value,'$[1]') AS depth, json_extract(value,'$[2]') AS id FROM json_each(?1)),
            combined AS (
                SELECT s.id,f.path,s.name,s.kind,s.start_line,s.end_line,s.parent,s.qualified,s.start_column,s.end_column,r.origin,r.depth
                    FROM reached r JOIN symbols s ON s.id=r.id JOIN files f ON f.id=s.file_id WHERE r.origin='current'
                UNION ALL
                SELECT s.symbol_id,s.file,s.name,s.kind,s.start_line,s.end_line,s.parent,s.qualified,s.start_column,s.end_column,r.origin,r.depth
                    FROM reached r JOIN snapshot_symbols s ON s.symbol_id=r.id AND s.generation=?2 WHERE r.origin='baseline'
            ) SELECT * FROM combined ORDER BY depth,path,start_line,origin,id LIMIT ?3 OFFSET ?4";
        let rows: Vec<Value> = self.conn.prepare(sql)?.query_map(params![rows_json,baseline,(window.limit+1) as i64,window.offset as i64],|r| Ok(json!({
            "symbol":symbol_from_row(r)?,"origin":r.get::<_,String>(10)?,"depth":r.get::<_,u32>(11)?
        })))?.collect::<rusqlite::Result<_>>()?;
        let mut value = serde_json::to_value(window.page(rows)?)?;
        value["baseline"] = json!(baseline);
        value["unsupported"] = json!(unsupported);
        let health: Option<crate::index::IndexStats> = self
            .meta("health")?
            .map(|s| serde_json::from_str(&s))
            .transpose()?;
        value["current_coverage_complete"] =
            json!(health.is_some_and(|h| h.scan_complete
                && h.files_failed == 0
                && h.parse_error_files == 0));
        value["baseline_available"] = json!(baseline.is_some());
        value["coverage"] =
            json!("structural changed ranges; old and new line coordinates may differ");
        Ok(value)
    }
}
