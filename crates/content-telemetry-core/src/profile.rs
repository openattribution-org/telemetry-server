//! Reporting-profile verification.
//!
//! The sink is a telemetry consumer under reporting profiles layered on the
//! Content Telemetry standard — first the RSL reporting profile at
//! `contenttelemetry.org/profiles/rsl/1`. Consumers never reject
//! non-conforming deliveries (standard, section 5.7.4: tolerate everything);
//! they *measure* them. This module evaluates delivered sessions against a
//! profile and reports where the delivery falls short of what the profile's
//! Client requirements oblige.
//!
//! The conformance surface is data-driven: [`ProfileDefinition`] is
//! deserialised from a JSON document (the RSL v1 draft ships embedded as
//! [`default_profile`]; deployments can point `CONFORMANCE_PROFILE_PATH` at
//! a revised file). A profile revision is a data change, not a code change
//! — the evaluator knows two rule shapes and everything else (which levels
//! oblige which event types, which fields on which events, the spec
//! references quoted back in reports) comes from the document.
//!
//! Two rule shapes cover the profile's technically verifiable requirements:
//!
//! - `level_event_coverage` — per session: the event types obliged by the
//!   session's claimed conformance level are all present (RSL profile
//!   s7.1; the levels map is data).
//! - `field_coverage` — per event: events of the listed types carry a
//!   field, either a wire-format column (`field`, e.g. `license_ref`) or a
//!   `data` member (`data_field`, e.g. `scope`), optionally restricted to
//!   events with a boolean `data` flag set (`when_data_flag`, e.g.
//!   `cached`).
//!
//! The profile's operational requirements — completeness (s7.3), cadence
//! (s7.4), endpoint conduct (s7.5) — cannot be verified from delivered
//! documents alone (profile, s9.1) and are deliberately absent here.
//! Observed delivery lag is reported alongside as evidence, never as a
//! pass/fail.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::conformance::normalise_conformance_level;

/// Embedded default: the RSL reporting profile v1 draft.
const DEFAULT_PROFILE_JSON: &str = include_str!("../profiles/rsl-1.json");

/// A reporting profile's verifiable surface, deserialised from JSON.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProfileDefinition {
    /// Immutable URI identifying the profile version (profile, section 10).
    pub profile_uri: String,
    /// The standard version the profile constrains.
    pub constrains: String,
    /// Level assumed when a session claims none. The RSL profile's
    /// empty-configuration default is `retrieval` (profile, section 6.2).
    pub default_claimed_level: String,
    /// Conformance level → what it obliges. Keys are level names in the
    /// standard's vocabulary; unknown claimed levels fall back to
    /// `default_claimed_level`.
    pub levels: BTreeMap<String, LevelRequirements>,
    pub rules: Vec<RuleDefinition>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LevelRequirements {
    pub required_event_types: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RuleDefinition {
    pub id: String,
    pub kind: RuleKind,
    /// Clause the rule verifies, quoted back in reports so a reader can
    /// check the requirement rather than trust the label.
    pub spec_ref: String,
    #[serde(default)]
    pub description: String,
    /// Wire-format column the rule requires (e.g. `license_ref`).
    #[serde(default)]
    pub field: Option<String>,
    /// `data` member the rule requires (e.g. `scope`). Exactly one of
    /// `field` / `data_field` applies to a `field_coverage` rule.
    #[serde(default)]
    pub data_field: Option<String>,
    /// Event types the rule applies to; empty means all content events.
    #[serde(default)]
    pub applies_to: Vec<String>,
    /// Restrict the rule to events whose `data` carries this flag as
    /// boolean `true` (e.g. `cached`).
    #[serde(default)]
    pub when_data_flag: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind {
    LevelEventCoverage,
    FieldCoverage,
}

/// Errors loading a profile definition.
#[derive(Debug, thiserror::Error)]
pub enum ProfileLoadError {
    #[error("failed to read profile file: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid profile definition: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("profile default_claimed_level '{0}' is not in its levels map")]
    DefaultLevelUnknown(String),
}

impl ProfileDefinition {
    /// Parse and sanity-check a profile definition from JSON text.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileLoadError`] when the JSON is malformed or the
    /// definition is internally inconsistent.
    pub fn from_json(json: &str) -> Result<Self, ProfileLoadError> {
        let profile: Self = serde_json::from_str(json)?;
        if !profile.levels.contains_key(&profile.default_claimed_level) {
            return Err(ProfileLoadError::DefaultLevelUnknown(
                profile.default_claimed_level,
            ));
        }
        Ok(profile)
    }

    /// Load a profile from a file path.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileLoadError`] on unreadable files or invalid content.
    pub fn from_path(path: &str) -> Result<Self, ProfileLoadError> {
        Self::from_json(&std::fs::read_to_string(path)?)
    }

    /// Resolve a session's claimed level to one the profile knows,
    /// normalising legacy values and falling back to the profile default.
    #[must_use]
    pub fn resolve_level(&self, claimed: Option<&str>) -> String {
        let normalised = normalise_conformance_level(claimed).value;
        match normalised {
            Some(level) if self.levels.contains_key(&level) => level,
            _ => self.default_claimed_level.clone(),
        }
    }
}

/// The embedded RSL v1 draft profile. Panics are impossible: the embedded
/// document is covered by tests.
#[must_use]
pub fn default_profile() -> ProfileDefinition {
    ProfileDefinition::from_json(DEFAULT_PROFILE_JSON)
        .expect("embedded profile definition is valid")
}

/// One delivered event, reduced to what profile rules can see.
#[derive(Debug, Clone)]
pub struct ProfileEvent {
    pub event_type: String,
    /// Wire-format columns present and non-empty (e.g. `license_ref`).
    pub fields_present: BTreeSet<String>,
    /// `data` members present and non-null (e.g. `scope`).
    pub data_fields_present: BTreeSet<String>,
    /// `data` members set to boolean `true` (e.g. `cached`).
    pub data_flags: BTreeSet<String>,
}

impl ProfileEvent {
    /// Build from the columns the sink stores per event.
    #[must_use]
    pub fn from_stored(
        event_type: &str,
        license_ref: Option<&str>,
        event_data: &serde_json::Value,
    ) -> Self {
        let mut fields_present = BTreeSet::new();
        if license_ref.is_some_and(|v| !v.is_empty()) {
            fields_present.insert("license_ref".to_string());
        }

        let mut data_fields_present = BTreeSet::new();
        let mut data_flags = BTreeSet::new();
        if let Some(obj) = event_data.as_object() {
            for (key, value) in obj {
                if !value.is_null() {
                    data_fields_present.insert(key.clone());
                }
                if value == &serde_json::Value::Bool(true) {
                    data_flags.insert(key.clone());
                }
            }
        }

        Self {
            event_type: event_type.to_string(),
            fields_present,
            data_fields_present,
            data_flags,
        }
    }
}

/// Outcome of one rule over one session.
#[derive(Debug, Clone, Serialize)]
pub struct RuleOutcome {
    pub rule_id: String,
    pub kind: RuleKind,
    pub spec_ref: String,
    /// Session-level verdict; `None` for event-level rules with no
    /// applicable events in the session.
    pub session_pass: Option<bool>,
    /// For `level_event_coverage`: obliged event types the session lacks.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub missing_event_types: Vec<String>,
    /// For `field_coverage`: applicable events seen / carrying the field.
    pub events_total: u64,
    pub events_passing: u64,
}

/// A session evaluated against a profile.
#[derive(Debug, Clone, Serialize)]
pub struct SessionEvaluation {
    /// The level the session was held to (claimed, normalised, defaulted).
    pub level: String,
    pub outcomes: Vec<RuleOutcome>,
}

/// Evaluate one delivered session against the profile. Pure; never
/// rejects — a consumer tolerates everything and reports what it saw.
#[must_use]
pub fn evaluate_session(
    profile: &ProfileDefinition,
    claimed_level: Option<&str>,
    events: &[ProfileEvent],
) -> SessionEvaluation {
    let level = profile.resolve_level(claimed_level);
    let present_types: BTreeSet<&str> = events.iter().map(|e| e.event_type.as_str()).collect();

    let outcomes = profile
        .rules
        .iter()
        .map(|rule| match rule.kind {
            RuleKind::LevelEventCoverage => {
                let required = profile
                    .levels
                    .get(&level)
                    .map(|l| l.required_event_types.as_slice())
                    .unwrap_or_default();
                let missing: Vec<String> = required
                    .iter()
                    .filter(|t| !present_types.contains(t.as_str()))
                    .cloned()
                    .collect();
                RuleOutcome {
                    rule_id: rule.id.clone(),
                    kind: rule.kind,
                    spec_ref: rule.spec_ref.clone(),
                    session_pass: Some(missing.is_empty()),
                    missing_event_types: missing,
                    events_total: 0,
                    events_passing: 0,
                }
            }
            RuleKind::FieldCoverage => {
                let applicable = events.iter().filter(|e| {
                    (rule.applies_to.is_empty() || rule.applies_to.contains(&e.event_type))
                        && rule
                            .when_data_flag
                            .as_ref()
                            .is_none_or(|flag| e.data_flags.contains(flag))
                });
                let mut total = 0_u64;
                let mut passing = 0_u64;
                for event in applicable {
                    total += 1;
                    let has = match (&rule.field, &rule.data_field) {
                        (Some(f), _) => event.fields_present.contains(f),
                        (None, Some(d)) => event.data_fields_present.contains(d),
                        (None, None) => true,
                    };
                    if has {
                        passing += 1;
                    }
                }
                RuleOutcome {
                    rule_id: rule.id.clone(),
                    kind: rule.kind,
                    spec_ref: rule.spec_ref.clone(),
                    session_pass: (total > 0).then_some(passing == total),
                    missing_event_types: Vec::new(),
                    events_total: total,
                    events_passing: passing,
                }
            }
        })
        .collect();

    SessionEvaluation { level, outcomes }
}

/// Per-rule aggregate across a set of evaluated sessions.
#[derive(Debug, Clone, Serialize)]
pub struct RuleAggregate {
    pub rule_id: String,
    pub kind: RuleKind,
    pub spec_ref: String,
    /// Sessions where the rule produced a verdict.
    pub sessions_with_verdict: u64,
    pub sessions_passing: u64,
    /// Event tallies for `field_coverage` rules; zero otherwise.
    pub events_total: u64,
    pub events_passing: u64,
    /// For `level_event_coverage`: obliged event type → sessions lacking it.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub missing_event_types: BTreeMap<String, u64>,
}

impl RuleAggregate {
    fn from_rule(rule: &RuleDefinition) -> Self {
        Self {
            rule_id: rule.id.clone(),
            kind: rule.kind,
            spec_ref: rule.spec_ref.clone(),
            sessions_with_verdict: 0,
            sessions_passing: 0,
            events_total: 0,
            events_passing: 0,
            missing_event_types: BTreeMap::new(),
        }
    }

    fn add(&mut self, outcome: &RuleOutcome) {
        if let Some(pass) = outcome.session_pass {
            self.sessions_with_verdict += 1;
            if pass {
                self.sessions_passing += 1;
            }
        }
        self.events_total += outcome.events_total;
        self.events_passing += outcome.events_passing;
        for t in &outcome.missing_event_types {
            *self.missing_event_types.entry(t.clone()).or_insert(0) += 1;
        }
    }
}

/// Observed delivery lag between an event's own timestamp and its arrival
/// at the sink. Evidence for the profile's cadence requirement (s7.4),
/// which remains operationally assessed — this is reported, never judged.
#[derive(Debug, Clone, Serialize)]
pub struct LagStats {
    pub samples: u64,
    pub p50_seconds: f64,
    pub p95_seconds: f64,
}

impl LagStats {
    /// Compute from raw lag samples (seconds). Returns `None` when empty.
    #[must_use]
    pub fn from_samples(mut samples: Vec<f64>) -> Option<Self> {
        if samples.is_empty() {
            return None;
        }
        samples.sort_by(f64::total_cmp);
        let pick = |p: f64| {
            #[allow(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss
            )]
            let idx = ((samples.len() - 1) as f64 * p).round() as usize;
            samples[idx]
        };
        Some(Self {
            samples: samples.len() as u64,
            p50_seconds: pick(0.5),
            p95_seconds: pick(0.95),
        })
    }
}

/// One agent's conformance aggregate over the evaluated window.
#[derive(Debug, Clone, Serialize)]
pub struct AgentConformance {
    pub platform_id: Option<String>,
    pub agent_id: Option<String>,
    pub sessions_evaluated: u64,
    /// Resolved conformance level → session count. Level transparency
    /// (profile s8.4): a consumer never represents telemetry as more
    /// complete than it is.
    pub levels: BTreeMap<String, u64>,
    pub rules: Vec<RuleAggregate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery_lag: Option<LagStats>,
}

/// Streaming aggregator: fold per-session evaluations into per-agent
/// aggregates without holding every evaluation in memory.
#[derive(Debug)]
pub struct AgentAggregator {
    profile_rules: Vec<RuleDefinition>,
    agents: BTreeMap<(Option<String>, Option<String>), AgentConformance>,
    lags: BTreeMap<(Option<String>, Option<String>), Vec<f64>>,
}

impl AgentAggregator {
    #[must_use]
    pub fn new(profile: &ProfileDefinition) -> Self {
        Self {
            profile_rules: profile.rules.clone(),
            agents: BTreeMap::new(),
            lags: BTreeMap::new(),
        }
    }

    pub fn add_session(
        &mut self,
        platform_id: Option<&str>,
        agent_id: Option<&str>,
        evaluation: &SessionEvaluation,
        lag_samples: &[f64],
    ) {
        let key = (
            platform_id.map(ToString::to_string),
            agent_id.map(ToString::to_string),
        );
        let entry = self
            .agents
            .entry(key.clone())
            .or_insert_with(|| AgentConformance {
                platform_id: platform_id.map(ToString::to_string),
                agent_id: agent_id.map(ToString::to_string),
                sessions_evaluated: 0,
                levels: BTreeMap::new(),
                rules: self
                    .profile_rules
                    .iter()
                    .map(RuleAggregate::from_rule)
                    .collect(),
                delivery_lag: None,
            });
        entry.sessions_evaluated += 1;
        *entry.levels.entry(evaluation.level.clone()).or_insert(0) += 1;
        for outcome in &evaluation.outcomes {
            if let Some(agg) = entry
                .rules
                .iter_mut()
                .find(|r| r.rule_id == outcome.rule_id)
            {
                agg.add(outcome);
            }
        }
        self.lags
            .entry(key)
            .or_default()
            .extend_from_slice(lag_samples);
    }

    #[must_use]
    pub fn finish(mut self) -> Vec<AgentConformance> {
        for (key, samples) in self.lags {
            if let Some(agent) = self.agents.get_mut(&key) {
                agent.delivery_lag = LagStats::from_samples(samples);
            }
        }
        self.agents.into_values().collect()
    }
}

/// The publisher-facing conformance report.
#[derive(Debug, Clone, Serialize)]
pub struct ConformanceReport {
    pub profile_uri: String,
    pub constrains: String,
    /// Sessions evaluated across all agents, bounded by the query's
    /// session limit — a sample, not a census, when it equals the limit.
    pub sessions_evaluated: u64,
    pub session_limit: i64,
    pub agents: Vec<AgentConformance>,
    /// The requirements this report cannot verify from delivered documents
    /// (profile s9.1) — completeness, cadence, endpoint conduct — restated
    /// so the report never overclaims what it proves.
    pub operationally_assessed: &'static str,
}

/// The fixed honesty note carried on every report: technical conformance is
/// runnable, operational requirements remain assessed.
pub const OPERATIONALLY_ASSESSED: &str = "Completeness (profile s7.3), cadence (s7.4) and \
     endpoint conduct (s7.5) cannot be verified from delivered documents and are assessed \
     by inspection and attestation; delivery_lag is observational evidence only.";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(event_type: &str, license_ref: Option<&str>, data: serde_json::Value) -> ProfileEvent {
        ProfileEvent::from_stored(event_type, license_ref, &data)
    }

    #[test]
    fn embedded_default_profile_parses_and_is_consistent() {
        let profile = default_profile();
        assert_eq!(
            profile.profile_uri,
            "https://contenttelemetry.org/profiles/rsl/1"
        );
        assert!(profile.levels.contains_key("retrieval"));
        assert!(profile.levels.contains_key("grounding"));
        assert!(profile.levels.contains_key("citation"));
        assert!(!profile.rules.is_empty());
    }

    #[test]
    fn default_level_outside_levels_map_is_rejected() {
        let err = ProfileDefinition::from_json(
            r#"{
                "profile_uri": "https://example.com/p/1",
                "constrains": "CT 1.0",
                "default_claimed_level": "platinum",
                "levels": { "retrieval": { "required_event_types": [] } },
                "rules": []
            }"#,
        )
        .unwrap_err();
        assert!(matches!(err, ProfileLoadError::DefaultLevelUnknown(_)));
    }

    #[test]
    fn citation_session_with_full_lifecycle_passes_level_coverage() {
        let profile = default_profile();
        let events = vec![
            ev("content_retrieved", Some("rsl:tok-1"), json!({})),
            ev(
                "content_grounded",
                Some("rsl:tok-1"),
                json!({"scope": "turn"}),
            ),
            ev(
                "content_cited",
                Some("rsl:tok-1"),
                json!({"citation_type": "direct_quote"}),
            ),
            ev("turn_started", None, json!({})),
            ev("turn_completed", None, json!({})),
        ];
        let result = evaluate_session(&profile, Some("citation"), &events);
        assert_eq!(result.level, "citation");
        let level_rule = result
            .outcomes
            .iter()
            .find(|o| o.rule_id == "level_event_coverage")
            .unwrap();
        assert_eq!(level_rule.session_pass, Some(true));
        assert!(level_rule.missing_event_types.is_empty());
    }

    #[test]
    fn citation_claim_without_cited_events_fails_level_coverage() {
        let profile = default_profile();
        let events = vec![
            ev("content_retrieved", None, json!({})),
            ev("content_grounded", None, json!({"scope": "turn"})),
            ev("turn_started", None, json!({})),
            ev("turn_completed", None, json!({})),
        ];
        let result = evaluate_session(&profile, Some("citation"), &events);
        let level_rule = result
            .outcomes
            .iter()
            .find(|o| o.rule_id == "level_event_coverage")
            .unwrap();
        assert_eq!(level_rule.session_pass, Some(false));
        assert_eq!(level_rule.missing_event_types, vec!["content_cited"]);
    }

    #[test]
    fn unclaimed_level_is_held_to_profile_default() {
        let profile = default_profile();
        let events = vec![ev("content_retrieved", None, json!({}))];
        let result = evaluate_session(&profile, None, &events);
        assert_eq!(result.level, "retrieval");
        let level_rule = result
            .outcomes
            .iter()
            .find(|o| o.rule_id == "level_event_coverage")
            .unwrap();
        assert_eq!(level_rule.session_pass, Some(true));
    }

    #[test]
    fn legacy_attribution_level_is_held_to_citation() {
        let profile = default_profile();
        let result = evaluate_session(&profile, Some("attribution"), &[]);
        assert_eq!(result.level, "citation");
    }

    #[test]
    fn license_linkage_counts_content_events_only() {
        let profile = default_profile();
        let events = vec![
            ev("content_retrieved", Some("rsl:tok-1"), json!({})),
            ev("content_retrieved", None, json!({})),
            ev("turn_completed", None, json!({})),
        ];
        let result = evaluate_session(&profile, Some("retrieval"), &events);
        let linkage = result
            .outcomes
            .iter()
            .find(|o| o.rule_id == "license_linkage")
            .unwrap();
        assert_eq!(linkage.events_total, 2);
        assert_eq!(linkage.events_passing, 1);
        assert_eq!(linkage.session_pass, Some(false));
    }

    #[test]
    fn cached_grounding_rule_applies_to_flagged_events_only() {
        let profile = default_profile();
        let events = vec![
            ev(
                "content_grounded",
                None,
                json!({"cached": true, "scope": "turn"}),
            ),
            ev(
                "content_grounded",
                Some("rsl:tok-1"),
                json!({"scope": "turn"}),
            ),
        ];
        let result = evaluate_session(&profile, Some("grounding"), &events);
        let cached = result
            .outcomes
            .iter()
            .find(|o| o.rule_id == "cached_grounding_linkage")
            .unwrap();
        // Only the cached grounding is in scope, and it lacks license_ref.
        assert_eq!(cached.events_total, 1);
        assert_eq!(cached.events_passing, 0);
        assert_eq!(cached.session_pass, Some(false));
    }

    #[test]
    fn field_rules_with_no_applicable_events_have_no_verdict() {
        let profile = default_profile();
        let events = vec![ev("content_retrieved", Some("rsl:tok-1"), json!({}))];
        let result = evaluate_session(&profile, Some("retrieval"), &events);
        let citation_rule = result
            .outcomes
            .iter()
            .find(|o| o.rule_id == "citation_type")
            .unwrap();
        assert_eq!(citation_rule.session_pass, None);
        assert_eq!(citation_rule.events_total, 0);
    }

    #[test]
    fn aggregator_folds_sessions_per_agent() {
        let profile = default_profile();
        let mut agg = AgentAggregator::new(&profile);

        let passing = evaluate_session(
            &profile,
            Some("retrieval"),
            &[ev("content_retrieved", Some("rsl:tok"), json!({}))],
        );
        let failing = evaluate_session(
            &profile,
            Some("citation"),
            &[ev("content_retrieved", None, json!({}))],
        );

        agg.add_session(Some("astral"), Some("Astral-User"), &passing, &[1.0, 3.0]);
        agg.add_session(Some("astral"), Some("Astral-User"), &failing, &[10.0]);
        agg.add_session(Some("corvid"), Some("Corvid-User"), &passing, &[]);

        let agents = agg.finish();
        assert_eq!(agents.len(), 2);

        let astral = agents
            .iter()
            .find(|a| a.agent_id.as_deref() == Some("Astral-User"))
            .unwrap();
        assert_eq!(astral.sessions_evaluated, 2);
        assert_eq!(astral.levels.get("retrieval"), Some(&1));
        assert_eq!(astral.levels.get("citation"), Some(&1));
        let level_agg = astral
            .rules
            .iter()
            .find(|r| r.rule_id == "level_event_coverage")
            .unwrap();
        assert_eq!(level_agg.sessions_with_verdict, 2);
        assert_eq!(level_agg.sessions_passing, 1);
        assert!(level_agg.missing_event_types.contains_key("content_cited"));
        assert_eq!(astral.delivery_lag.as_ref().unwrap().samples, 3);

        let corvid = agents
            .iter()
            .find(|a| a.agent_id.as_deref() == Some("Corvid-User"))
            .unwrap();
        assert!(corvid.delivery_lag.is_none());
    }

    #[test]
    fn lag_percentiles_from_samples() {
        let stats = LagStats::from_samples(vec![5.0, 1.0, 2.0, 3.0, 4.0]).unwrap();
        assert_eq!(stats.samples, 5);
        assert!((stats.p50_seconds - 3.0).abs() < f64::EPSILON);
        assert!((stats.p95_seconds - 5.0).abs() < f64::EPSILON);
        assert!(LagStats::from_samples(vec![]).is_none());
    }

    #[test]
    fn revised_profile_changes_behaviour_without_code_changes() {
        // The data-driven claim, tested: a profile revision that drops the
        // turn-event obligation changes the verdict for the same delivery.
        let revised = ProfileDefinition::from_json(
            r#"{
                "profile_uri": "https://example.com/profiles/rsl/2",
                "constrains": "CT 2.0",
                "default_claimed_level": "retrieval",
                "levels": {
                    "retrieval": { "required_event_types": ["content_retrieved"] },
                    "grounding": { "required_event_types": ["content_retrieved", "content_grounded"] }
                },
                "rules": [
                    { "id": "level_event_coverage", "kind": "level_event_coverage",
                      "spec_ref": "revised s7.1" }
                ]
            }"#,
        )
        .unwrap();

        let events = vec![
            ev("content_retrieved", None, json!({})),
            ev("content_grounded", None, json!({"scope": "turn"})),
        ];

        let against_default = evaluate_session(&default_profile(), Some("grounding"), &events);
        let against_revised = evaluate_session(&revised, Some("grounding"), &events);

        let default_verdict = against_default
            .outcomes
            .iter()
            .find(|o| o.rule_id == "level_event_coverage")
            .unwrap();
        let revised_verdict = against_revised
            .outcomes
            .iter()
            .find(|o| o.rule_id == "level_event_coverage")
            .unwrap();

        // Same events: the v1 draft demands turn events at grounding, the
        // revision does not.
        assert_eq!(default_verdict.session_pass, Some(false));
        assert_eq!(revised_verdict.session_pass, Some(true));
    }
}
