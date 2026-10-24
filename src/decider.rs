//! OpenRouter Decisions API client.
//!
//! Endpoint: `POST {base_url}` (default
//! `https://openrouter.ai/api/alpha/decisions`) with
//! `Authorization: Bearer $OPENROUTER_API_KEY`.
//! Body: `{model, state, questions}` — the same contract as the Perplexity
//! Decisions API. Response: `{answers: {...}, model, usage}`.

use std::{collections::HashMap, time::Duration};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct DeciderClient {
    base_url: String,
    model: String,
    api_key: String,
    app_title: String,
    http_referer: Option<String>,
    timeout: Duration,
    http: reqwest::Client,
}

#[derive(Debug, Clone, Serialize)]
struct DecisionsRequest {
    model: String,
    state: serde_json::Value,
    questions: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DecisionsResponse {
    #[serde(default)]
    pub answers: HashMap<String, Answer>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub usage: Option<serde_json::Value>,
}

/// One typed answer. Only the fields for the question's type are populated.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Answer {
    /// `noul`: probability of yes, 0..1
    #[serde(default)]
    pub noul: Option<f64>,
    /// `choice`: winning option name
    #[serde(default)]
    pub choice: Option<String>,
    /// `choice`/`score`: per-option/level probabilities
    #[serde(default)]
    pub probabilities: Option<HashMap<String, f64>>,
    /// `score`: probability-weighted expected level index
    #[serde(default)]
    pub score: Option<f64>,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub legend: Option<HashMap<String, String>>,
}

impl DeciderClient {
    pub fn new(
        base_url: String,
        model: String,
        api_key: String,
        app_title: String,
        http_referer: Option<String>,
        timeout_secs: u64,
    ) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs.max(5)))
            .build()
            .context("building HTTP client")?;
        Ok(Self {
            base_url,
            model,
            api_key,
            app_title,
            http_referer,
            timeout: Duration::from_secs(timeout_secs.max(5)),
            http,
        })
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// Classify one message. `state` is the Decisions-API state object,
    /// `questions` the verbatim value from the policy file.
    pub async fn classify(
        &self,
        state: serde_json::Value,
        questions: serde_json::Value,
    ) -> Result<DecisionsResponse> {
        let body = DecisionsRequest {
            model: self.model.clone(),
            state,
            questions,
        };
        let mut req = self
            .http
            .post(&self.base_url)
            .bearer_auth(&self.api_key)
            .header("Content-Type", "application/json")
            .header("X-Title", &self.app_title);
        if let Some(r) = &self.http_referer {
            req = req.header("HTTP-Referer", r);
        }
        let resp = req
            .json(&body)
            .timeout(self.timeout)
            .send()
            .await
            .context("sending Decisions request to OpenRouter")?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            let short = truncate(&text, 500);
            anyhow::bail!("OpenRouter Decisions API error {status}: {short}");
        }
        let parsed: DecisionsResponse =
            resp.json().await.context("parsing Decisions response")?;
        Ok(parsed)
    }
}

/// Simplified classification outcome used by the enforcement engine.
/// The bot never invents criteria — it only reads these numbers.
#[derive(Debug, Clone, Default)]
pub struct Classification {
    pub spam_score: f64,
    pub language_score: f64,
    pub category: String,
    pub category_probs: HashMap<String, f64>,
    pub model: String,
    pub raw: serde_json::Value,
}

impl Classification {
    pub fn from_response(
        resp: &DecisionsResponse,
        raw: serde_json::Value,
        spam_q: &str,
        lang_q: &str,
        cat_q: &str,
        model: &str,
    ) -> Self {
        let spam_score = resp
            .answers
            .get(spam_q)
            .and_then(|a| a.noul)
            .unwrap_or(0.0)
            .clamp(0.0, 1.0);
        let language_score = resp
            .answers
            .get(lang_q)
            .and_then(|a| a.noul)
            .unwrap_or(0.0)
            .clamp(0.0, 1.0);
        let (category, category_probs) = match resp.answers.get(cat_q) {
            Some(a) => (
                a.choice.clone().unwrap_or_else(|| "unknown".to_string()),
                a.probabilities.clone().unwrap_or_default(),
            ),
            None => ("unknown".to_string(), HashMap::new()),
        };
        Self {
            spam_score,
            language_score,
            category,
            category_probs,
            model: resp.model.clone().unwrap_or_else(|| model.to_string()),
            raw,
        }
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    format!("{}…", &s[..n])
}

/// Build the `state` object for one message. The policy file's questions all
/// read this same state.
pub fn message_state(
    body: &str,
    author: &str,
    room_id: &str,
    max_chars: usize,
) -> serde_json::Value {
    let mut text = body.to_string();
    if text.len() > max_chars {
        text.truncate(max_chars);
    }
    serde_json::json!({
        "message": text,
        "author": author,
        "room": room_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_noul_choice_answers() {
        let v = serde_json::json!({
            "answers": {
                "spam": {"noul": 0.97, "confidence": 0.9},
                "spam_category": {"choice": "investment",
                    "probabilities": {"investment": 0.8, "legitimate": 0.2},
                    "confidence": 0.8}
            },
            "model": "perplexity/pplx-decider-v1.2"
        });
        let resp: DecisionsResponse = serde_json::from_value(v).unwrap();
        let cls = Classification::from_response(
            &resp,
            serde_json::json!({}),
            "spam",
            "language_violation",
            "spam_category",
            "m",
        );
        assert!((cls.spam_score - 0.97).abs() < 1e-9);
        assert_eq!(cls.category, "investment");
        assert!((cls.category_probs["investment"] - 0.8).abs() < 1e-9);
    }

    #[test]
    fn missing_questions_default_to_zero() {
        let resp = DecisionsResponse {
            answers: HashMap::new(),
            model: None,
            usage: None,
        };
        let cls = Classification::from_response(
            &resp,
            serde_json::json!({}),
            "spam",
            "language_violation",
            "spam_category",
            "m",
        );
        assert_eq!(cls.spam_score, 0.0);
        assert_eq!(cls.category, "unknown");
    }

    #[test]
    fn state_truncates_long_messages() {
        let s = message_state(&"x".repeat(5000), "@a:b", "!r:b", 100);
        assert!(s["message"].as_str().unwrap().len() <= 100);
    }

    // Live HTTP test against a mock server (wiremock), not the real API.
    #[tokio::test]
    async fn classify_posts_to_openrouter_shape() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use wiremock::matchers::{header, method, path};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/alpha/decisions"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "answers": {"spam": {"noul": 0.12}},
                "model": "perplexity/pplx-decider-v1.2"
            })))
            .mount(&server)
            .await;
        let client = DeciderClient::new(
            format!("{}/api/alpha/decisions", server.uri()),
            "perplexity/pplx-decider-v1.2".to_string(),
            "test-key".to_string(),
            "mirsad-test".to_string(),
            None,
            10,
        )
        .unwrap();
        let resp = client
            .classify(
                serde_json::json!({"message": "hello"}),
                serde_json::json!({"spam": {"type": "noul", "instructions": "spam?"}}),
            )
            .await
            .unwrap();
        assert!((resp.answers["spam"].noul.unwrap() - 0.12).abs() < 1e-9);
    }
}
