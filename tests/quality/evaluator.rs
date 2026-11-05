//! The oracle consists of hand-authored labels in synthetic source, never
//! output copied from an indexer. Marker positions become UTF-8 token ranges.
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct Location {
    pub file: String,
    pub range: [u32; 4],
}

#[derive(Debug)]
pub struct Case {
    pub id: String,
    pub family: String,
    pub site: Location,
    pub expected: Option<Location>,
}

pub type Observations = BTreeMap<Location, BTreeSet<Location>>;

#[derive(Debug, Default, PartialEq)]
pub struct Score {
    pub eligible: usize,
    pub true_positive: usize,
    pub false_positive: usize,
    pub false_negative: usize,
    pub safety_failures: usize,
}

impl Score {
    pub fn passes(&self) -> bool {
        // Integer comparisons avoid rounding a failing score up to the gate.
        self.eligible >= 100
            && self.true_positive * 100 >= self.eligible * 90
            && self.true_positive * 100 >= (self.true_positive + self.false_positive) * 99
            && self.safety_failures == 0
    }
}

pub fn score(cases: &[Case], observations: &Observations) -> Score {
    let mut result = Score::default();
    let mut known = BTreeSet::new();
    for case in cases {
        known.insert(&case.site);
        let actual = observations.get(&case.site);
        if let Some(expected) = &case.expected {
            result.eligible += 1;
            if actual.is_some_and(|targets| targets.contains(expected)) {
                result.true_positive += 1;
            } else {
                result.false_negative += 1;
            }
            result.false_positive += actual.map_or(0, |targets| {
                targets.iter().filter(|t| *t != expected).count()
            });
        } else if actual.is_some_and(|targets| !targets.is_empty()) {
            result.safety_failures += 1;
            result.false_positive += actual.map_or(0, BTreeSet::len);
        }
    }
    // Unlabelled returned call sites cannot silently improve precision.
    result.false_positive += observations
        .iter()
        .filter(|(site, _)| !known.contains(site))
        .map(|(_, targets)| targets.len())
        .sum::<usize>();
    result
}

pub fn parse(files: &[(&str, &str)]) -> Result<Vec<Case>, String> {
    let mut definitions = BTreeMap::new();
    let mut calls = Vec::new();
    let mut ids = BTreeSet::new();
    let mut sites = BTreeSet::new();
    for (file, source) in files {
        let mut offset = 0;
        while let Some(relative) = source[offset..].find("/*@") {
            let start = offset + relative + 3;
            let end = start + source[start..].find("*/").ok_or("unterminated label")?;
            let (kind, label) = source[start..end]
                .split_once(':')
                .ok_or("label needs kind")?;
            let after = end + 2;
            let rest = source[after..].trim_start();
            let token = source.len() - rest.len();
            let length = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .map(char::len_utf8)
                .sum::<usize>();
            if length == 0 {
                return Err(format!("{file}: label must precede a token"));
            }
            let prefix = &source[..token];
            let line = prefix.bytes().filter(|&c| c == b'\n').count() as u32;
            let column = prefix.rsplit('\n').next().unwrap_or_default().len() as u32;
            let location = Location {
                file: (*file).into(),
                range: [line, column, line, column + length as u32],
            };
            match kind {
                "def" => {
                    if definitions.insert(label.to_owned(), location).is_some() {
                        return Err(format!("duplicate definition: {label}"));
                    }
                }
                "call" | "deny" => {
                    let (id, target) = if kind == "call" {
                        let (id, target) = label.split_once("=>").ok_or("call needs target")?;
                        (id, Some(target.to_owned()))
                    } else {
                        (label, None)
                    };
                    if !ids.insert(id.to_owned()) || !sites.insert(location.clone()) {
                        return Err(format!("duplicate call: {id}"));
                    }
                    let (family, _) = id.split_once('.').ok_or("case id needs family")?;
                    calls.push((id.to_owned(), family.to_owned(), location, target));
                }
                _ => return Err(format!("unknown label kind: {kind}")),
            }
            offset = after;
        }
    }
    calls
        .into_iter()
        .map(|(id, family, site, target)| {
            let expected = target
                .map(|name| {
                    definitions
                        .get(&name)
                        .cloned()
                        .ok_or_else(|| format!("{id}: unknown target {name}"))
                })
                .transpose()?;
            Ok(Case {
                id,
                family,
                site,
                expected,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oracle_rejects_bad_labels_and_tracks_utf8_columns() {
        let cases = parse(&[(
            "fixture.rs",
            "/* é */ fn /*@def:f*/fun() {}\r\n/*@call:local.one=>f*/fun();",
        )])
        .unwrap();
        assert_eq!(cases[0].id, "local.one");
        assert_eq!(cases[0].family, "local");
        assert_eq!(cases[0].site.range, [1, 22, 1, 25]);
        assert_eq!(cases[0].expected.as_ref().unwrap().range, [0, 22, 0, 25]);
        for source in [
            "/*@call:x.1=>missing*/f()",
            "/*@def:f*/f /*@def:f*/g",
            "/*@call:x.1=>f*/f /*@call:x.1=>f*/f",
            "/*@deny:x.1*/()",
        ] {
            assert!(parse(&[("fixture.rs", source)]).is_err(), "{source}");
        }
    }

    #[test]
    fn missing_producer_wrong_targets_and_safety_errors_fail_the_gate() {
        let target = Location {
            file: "target.rs".into(),
            range: [0, 0, 0, 1],
        };
        let mut cases: Vec<_> = (0..100)
            .map(|i| Case {
                id: format!("family.{i}"),
                family: "family".into(),
                site: Location {
                    file: "calls.rs".into(),
                    range: [i, 0, i, 1],
                },
                expected: Some(target.clone()),
            })
            .collect();
        let missing = score(&cases, &Observations::new());
        assert_eq!(missing.false_negative, 100);
        assert!(!missing.passes(), "missing producer is failure, not a skip");
        let mut observed: Observations = cases
            .iter()
            .map(|c| (c.site.clone(), BTreeSet::from([target.clone()])))
            .collect();
        assert!(score(&cases, &observed).passes());
        for case in &cases[..11] {
            observed.remove(&case.site);
        }
        assert!(
            !score(&cases, &observed).passes(),
            "89 percent is below target"
        );
        observed.insert(cases[10].site.clone(), BTreeSet::from([target.clone()]));
        assert!(
            score(&cases, &observed).passes(),
            "90 percent passes recall"
        );
        let wrong = Location {
            file: "unrelated.rs".into(),
            range: [0, 0, 0, 1],
        };
        observed.insert(cases[99].site.clone(), BTreeSet::from([wrong.clone()]));
        let bad = score(&cases, &observed);
        assert_eq!(bad.false_positive, 1);
        assert_eq!(
            bad.false_negative, 11,
            "wrong targets also miss the expected target"
        );
        assert!(!bad.passes());
        observed = cases
            .iter()
            .map(|c| (c.site.clone(), BTreeSet::from([target.clone()])))
            .collect();
        for case in &cases[..2] {
            observed.insert(case.site.clone(), BTreeSet::from([wrong.clone()]));
        }
        let bad_precision = score(&cases, &observed);
        assert_eq!(bad_precision.true_positive, 98);
        assert!(
            !bad_precision.passes(),
            "98% recall cannot hide precision below 99%"
        );
        observed = cases
            .iter()
            .map(|c| (c.site.clone(), BTreeSet::from([target.clone()])))
            .collect();
        let denied = Location {
            file: "dynamic.rs".into(),
            range: [0, 0, 0, 1],
        };
        cases.push(Case {
            id: "safety.dynamic".into(),
            family: "safety".into(),
            site: denied.clone(),
            expected: None,
        });
        observed.insert(denied, BTreeSet::from([target]));
        assert_eq!(score(&cases, &observed).safety_failures, 1);
        assert!(
            !score(&cases, &observed).passes(),
            "a safety failure blocks otherwise passing metrics"
        );
    }
}
