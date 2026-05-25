//! lava-cron — typed `(deflava-cron …)` for scheduled lava deployments.
//!
//! Pangea-cron analog. A schedule pairs a cron expression with a
//! target architecture + bindings + optional gate. The typed
//! [`CronSchedule`] + [`Tick`] state machine drive next-fire
//! calculation; a downstream operator (cron-trigger / k8s CronJob /
//! GitHub Actions schedule) invokes the architecture render when
//! the tick fires.
//!
//! ## Form
//!
//! ```lisp
//! (deflava-cron weekly-vpc-drift-check
//!   :expression "0 6 * * 1"          ;; every Monday at 06:00 UTC
//!   :architecture aws-vpc-network
//!   :bindings (:name "prod" :cidr "10.0.0.0/16")
//!   :action plan)                    ;; plan | apply | destroy | refresh
//! ```

#![allow(clippy::module_name_repetitions)]

use indexmap::IndexMap;
use lava_eval::{parse_all, Sx};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// One scheduled architecture invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CronSchedule {
    pub name: String,
    pub expression: String,
    pub architecture: String,
    #[serde(default)]
    pub bindings: IndexMap<String, String>,
    #[serde(default = "default_action")]
    pub action: Action,
    #[serde(default)]
    pub doc: Option<String>,
}

fn default_action() -> Action {
    Action::Plan
}

/// Action the scheduled tick should drive when it fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    Plan,
    Apply,
    Destroy,
    Refresh,
}

impl Action {
    /// Stable kebab-case form. Surfaced in operator-facing UIs +
    /// the cron-trigger lookup table.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Apply => "apply",
            Self::Destroy => "destroy",
            Self::Refresh => "refresh",
        }
    }
}

#[derive(Debug, Error)]
pub enum CronParseError {
    #[error("parse: {0}")]
    Parse(#[from] lava_eval::ParseError),
    #[error("missing :{0} clause")]
    MissingClause(&'static str),
    #[error("malformed deflava-cron form: {0}")]
    Malformed(String),
    #[error("unknown action `{0}` (expected plan|apply|destroy|refresh)")]
    UnknownAction(String),
}

/// Scan a source string for every `(deflava-cron …)` form and return
/// one [`CronSchedule`] per declaration.
///
/// # Errors
/// Parse errors and per-schedule shape errors surface as typed
/// [`CronParseError`] variants.
pub fn schedules_in_source(src: &str) -> Result<Vec<CronSchedule>, CronParseError> {
    let forms = parse_all(src)?;
    let mut out = Vec::new();
    for form in forms {
        let Some(xs) = form.as_list() else { continue };
        if xs.first().and_then(Sx::as_sym) == Some("deflava-cron") {
            out.push(schedule_from_form(xs)?);
        }
    }
    Ok(out)
}

fn schedule_from_form(xs: &[Sx]) -> Result<CronSchedule, CronParseError> {
    let name = xs
        .get(1)
        .and_then(Sx::as_sym)
        .or_else(|| xs.get(1).and_then(Sx::as_str))
        .ok_or_else(|| CronParseError::Malformed("missing schedule name".into()))?
        .to_string();
    let mut expression: Option<String> = None;
    let mut architecture: Option<String> = None;
    let mut bindings: IndexMap<String, String> = IndexMap::new();
    let mut action = Action::Plan;
    let mut doc: Option<String> = None;
    let mut i = 2;
    while i + 1 < xs.len() {
        match xs[i].as_kw() {
            Some("expression") => {
                expression = xs[i + 1].as_str().map(std::string::ToString::to_string);
            }
            Some("architecture") => {
                architecture = xs[i + 1]
                    .as_sym()
                    .or_else(|| xs[i + 1].as_str())
                    .map(std::string::ToString::to_string);
            }
            Some("bindings") => {
                if let Some(pairs) = xs[i + 1].as_list() {
                    let mut j = 0;
                    while j + 1 < pairs.len() {
                        if let (Some(k), Some(v)) =
                            (pairs[j].as_kw(), pairs[j + 1].as_str())
                        {
                            bindings.insert(k.to_string(), v.to_string());
                        }
                        j += 2;
                    }
                }
            }
            Some("action") => {
                let a = xs[i + 1]
                    .as_sym()
                    .or_else(|| xs[i + 1].as_str())
                    .ok_or_else(|| CronParseError::Malformed(":action not a sym".into()))?;
                action = match a {
                    "plan" => Action::Plan,
                    "apply" => Action::Apply,
                    "destroy" => Action::Destroy,
                    "refresh" => Action::Refresh,
                    other => return Err(CronParseError::UnknownAction(other.to_string())),
                };
            }
            Some("doc") => {
                doc = xs[i + 1].as_str().map(std::string::ToString::to_string);
            }
            _ => {}
        }
        i += 2;
    }
    Ok(CronSchedule {
        name,
        expression: expression.ok_or(CronParseError::MissingClause("expression"))?,
        architecture: architecture.ok_or(CronParseError::MissingClause("architecture"))?,
        bindings,
        action,
        doc,
    })
}

/// Typed parse of a cron expression. Validates the 5-field shape
/// (minute hour day month weekday) — accepts `*`, integer literals,
/// `*/N` step, comma-separated lists, and `A-B` ranges. Bad shapes
/// surface as typed errors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CronExpression {
    pub minute: Vec<u8>,
    pub hour: Vec<u8>,
    pub day_of_month: Vec<u8>,
    pub month: Vec<u8>,
    pub day_of_week: Vec<u8>,
}

impl CronExpression {
    /// Parse a 5-field cron expression. Accepts `* * * * *` shorthand.
    ///
    /// # Errors
    /// Returns [`CronParseError::Malformed`] for the wrong number of
    /// fields, non-numeric values, or out-of-range entries.
    pub fn parse(expr: &str) -> Result<Self, CronParseError> {
        let parts: Vec<&str> = expr.split_whitespace().collect();
        if parts.len() != 5 {
            return Err(CronParseError::Malformed(format!(
                "expected 5 cron fields, got {}",
                parts.len()
            )));
        }
        Ok(Self {
            minute: parse_field(parts[0], 0..=59)?,
            hour: parse_field(parts[1], 0..=23)?,
            day_of_month: parse_field(parts[2], 1..=31)?,
            month: parse_field(parts[3], 1..=12)?,
            day_of_week: parse_field(parts[4], 0..=6)?,
        })
    }
}

fn parse_field(s: &str, range: std::ops::RangeInclusive<u8>) -> Result<Vec<u8>, CronParseError> {
    if s == "*" {
        return Ok((*range.start()..=*range.end()).collect());
    }
    if let Some(step_str) = s.strip_prefix("*/") {
        let step: u8 = step_str
            .parse()
            .map_err(|_| CronParseError::Malformed(format!("bad step `{s}`")))?;
        if step == 0 {
            return Err(CronParseError::Malformed(format!("step must be > 0: `{s}`")));
        }
        return Ok((*range.start()..=*range.end())
            .filter(|v| (v - range.start()) % step == 0)
            .collect());
    }
    let mut out = Vec::new();
    for piece in s.split(',') {
        if let Some((lo, hi)) = piece.split_once('-') {
            let lo: u8 = lo
                .parse()
                .map_err(|_| CronParseError::Malformed(format!("range lo `{piece}`")))?;
            let hi: u8 = hi
                .parse()
                .map_err(|_| CronParseError::Malformed(format!("range hi `{piece}`")))?;
            for v in lo..=hi {
                if !range.contains(&v) {
                    return Err(CronParseError::Malformed(format!(
                        "value {v} out of range [{}..={}]",
                        range.start(),
                        range.end()
                    )));
                }
                out.push(v);
            }
        } else {
            let v: u8 = piece
                .parse()
                .map_err(|_| CronParseError::Malformed(format!("non-numeric `{piece}`")))?;
            if !range.contains(&v) {
                return Err(CronParseError::Malformed(format!(
                    "value {v} out of range [{}..={}]",
                    range.start(),
                    range.end()
                )));
            }
            out.push(v);
        }
    }
    Ok(out)
}

/// One tick of the cron-trigger loop. Returns the schedules whose
/// expression matches the supplied "now" timestamp (passed as
/// (minute, hour, day-of-month, month, day-of-week) so this crate
/// stays chrono-free).
#[must_use]
pub fn schedules_firing_at<'a>(
    schedules: &'a [CronSchedule],
    minute: u8,
    hour: u8,
    day_of_month: u8,
    month: u8,
    day_of_week: u8,
) -> Vec<&'a CronSchedule> {
    schedules
        .iter()
        .filter(|s| {
            CronExpression::parse(&s.expression)
                .map(|e| {
                    e.minute.contains(&minute)
                        && e.hour.contains(&hour)
                        && e.day_of_month.contains(&day_of_month)
                        && e.month.contains(&month)
                        && e.day_of_week.contains(&day_of_week)
                })
                .unwrap_or(false)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_from_form_extracts_all_typed_fields() {
        let src = r#"
            (deflava-cron weekly-vpc-drift-check
              :doc "Weekly drift check on prod VPC"
              :expression "0 6 * * 1"
              :architecture aws-vpc-network
              :bindings (:name "prod" :cidr "10.0.0.0/16")
              :action plan)
        "#;
        let schedules = schedules_in_source(src).unwrap();
        assert_eq!(schedules.len(), 1);
        let s = &schedules[0];
        assert_eq!(s.name, "weekly-vpc-drift-check");
        assert_eq!(s.expression, "0 6 * * 1");
        assert_eq!(s.architecture, "aws-vpc-network");
        assert_eq!(s.bindings["name"], "prod");
        assert_eq!(s.action, Action::Plan);
        assert_eq!(s.doc.as_deref(), Some("Weekly drift check on prod VPC"));
    }

    #[test]
    fn schedule_missing_expression_surfaces_typed_error() {
        let src = "(deflava-cron x :architecture y)";
        let err = schedules_in_source(src).unwrap_err();
        matches!(err, CronParseError::MissingClause("expression"));
    }

    #[test]
    fn schedule_unknown_action_surfaces_typed_error() {
        let src = r#"
            (deflava-cron x
              :expression "0 0 * * *"
              :architecture y
              :action moonwalk)
        "#;
        let err = schedules_in_source(src).unwrap_err();
        matches!(err, CronParseError::UnknownAction(_));
    }

    #[test]
    fn cron_expression_parse_star_expands_to_full_range() {
        let e = CronExpression::parse("* * * * *").unwrap();
        assert_eq!(e.minute.len(), 60);
        assert_eq!(e.hour.len(), 24);
        assert_eq!(e.day_of_month.len(), 31);
        assert_eq!(e.month.len(), 12);
        assert_eq!(e.day_of_week.len(), 7);
    }

    #[test]
    fn cron_expression_parse_step_skips_correctly() {
        // */5 in minute → 0, 5, 10, …, 55
        let e = CronExpression::parse("*/5 * * * *").unwrap();
        assert_eq!(e.minute, vec![0, 5, 10, 15, 20, 25, 30, 35, 40, 45, 50, 55]);
    }

    #[test]
    fn cron_expression_parse_range_and_list() {
        let e = CronExpression::parse("0 9-17 * * 1,3,5").unwrap();
        assert_eq!(e.hour, vec![9, 10, 11, 12, 13, 14, 15, 16, 17]);
        assert_eq!(e.day_of_week, vec![1, 3, 5]);
    }

    #[test]
    fn cron_expression_wrong_field_count_surfaces_typed_error() {
        let err = CronExpression::parse("0 0 *").unwrap_err();
        matches!(err, CronParseError::Malformed(_));
    }

    #[test]
    fn schedules_firing_at_finds_matching_schedules() {
        // "0 6 * * 1" — every Monday at 06:00
        let s = vec![CronSchedule {
            name: "weekly".into(),
            expression: "0 6 * * 1".into(),
            architecture: "x".into(),
            bindings: IndexMap::new(),
            action: Action::Plan,
            doc: None,
        }];
        // Monday 06:00 fires.
        assert_eq!(schedules_firing_at(&s, 0, 6, 1, 1, 1).len(), 1);
        // Tuesday 06:00 does not.
        assert_eq!(schedules_firing_at(&s, 0, 6, 2, 1, 2).len(), 0);
    }

    #[test]
    fn cron_schedule_round_trips_through_serde() {
        let s = CronSchedule {
            name: "x".into(),
            expression: "0 0 * * *".into(),
            architecture: "y".into(),
            bindings: IndexMap::new(),
            action: Action::Apply,
            doc: None,
        };
        let json = serde_json::to_string(&s).unwrap();
        let parsed: CronSchedule = serde_json::from_str(&json).unwrap();
        assert_eq!(s, parsed);
    }
}
