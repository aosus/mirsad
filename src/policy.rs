//! External decision-policy file (`decision-questions.json`).
//!
//! The spam behaviour lives HERE, not in Rust code. The bot loads these
//! questions verbatim and sends them to the OpenRouter Decisions API with the
//! message body injected as `state`. Changing this file + restarting is enough
//! to retune the bot — no rebuild needed.

use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// One question as defined by the Decisions API:
/// `noul` (yes/no probability), `choice` (one of N options), `score` (rubric level).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct QuestionDef {
    #[serde(rename = "type")]
    pub qtype: String,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub criteria: Option<serde_json::Value>,
}

impl QuestionDef {
    pub fn validate(&self, name: &str) -> Result<()> {
        anyhow::ensure!(
            ["noul", "choice", "score"].contains(&self.qtype.as_str()),
            "question '{name}' has unknown type '{}'",
            self.qtype
        );
        match self.qtype.as_str() {
            "noul" => {
                anyhow::ensure!(
                    self.instructions.as_ref().is_some_and(|s| !s.is_empty())
                        || self.criteria.is_some(),
                    "noul question '{name}' needs instructions or criteria"
                );
            }
            "choice" => {
                let n = self
                    .criteria
                    .as_ref()
                    .and_then(|c| c.as_object())
                    .map(|o| o.len())
                    .unwrap_or(0);
                anyhow::ensure!((1..=255).contains(&n), "choice question '{name}' needs 1-255 options");
            }
            "score" => {
                let n = self
                    .criteria
                    .as_ref()
                    .and_then(|c| c.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                anyhow::ensure!((2..=10).contains(&n), "score question '{name}' needs 2-10 levels");
            }
            _ => unreachable!(),
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PolicyFile {
    #[serde(default)]
    pub questions: BTreeMap<String, QuestionDef>,
}

impl PolicyFile {
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let pf: PolicyFile =
            serde_json::from_str(&text).context("parsing decision-questions.json")?;
        pf.validate()?;
        Ok(pf)
    }

    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(!self.questions.is_empty(), "no questions defined");
        anyhow::ensure!(
            self.questions.len() <= 128,
            "too many questions (max 128 per Decisions request)"
        );
        for (name, q) in &self.questions {
            anyhow::ensure!(!name.is_empty(), "question name must be non-empty");
            q.validate(name)?;
        }
        Ok(())
    }

    /// Build the Decisions-API `questions` JSON value verbatim.
    pub fn as_api_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.questions).unwrap_or(serde_json::Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PolicyFile {
        serde_json::from_value(serde_json::json!({
            "questions": {
                "spam": {"type": "noul", "instructions": "Is this spam?"},
                "language_violation": {"type": "noul", "instructions": "Is this non-Arabic?"},
                "spam_category": {"type": "choice", "instructions": "Pick a category",
                    "criteria": {"investment": "x", "jobs": "y", "referral": "z", "other_ads": "w", "legitimate": null}}
            }
        }))
        .unwrap()
    }

    #[test]
    fn valid_policy_passes() {
        assert!(sample().validate().is_ok());
    }

    #[test]
    fn bad_type_rejected() {
        let mut p = sample();
        p.questions.get_mut("spam").unwrap().qtype = "chat".to_string();
        assert!(p.validate().is_err());
    }

    #[test]
    fn empty_questions_rejected() {
        let p = PolicyFile {
            questions: BTreeMap::new(),
        };
        assert!(p.validate().is_err());
    }
}
