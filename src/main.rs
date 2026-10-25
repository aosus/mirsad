//! Mirsad — autonomous AI spam moderation bot for Aosus Matrix rooms.
//!
//! Env secrets: MATRIX_PASSWORD (bot password), OPENROUTER_API_KEY.
//! Configs: `--config config.toml` + external decision-questions JSON file.

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use clap::Parser;
use mirsad::{config::Secrets, decider::DeciderClient, matrix, policy, store::Store};
use tokio::sync::Mutex;

#[derive(Debug, Parser)]
#[command(name = "mirsad", about = "Aosus AI spam moderation bot")]
struct Cli {
    /// Path to config.toml
    #[arg(long, default_value = "config/config.toml")]
    config: PathBuf,
    /// Validate configs and exit (no network needed, skips API key check)
    #[arg(long, default_value_t = false)]
    check_config: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mirsad=info".into()),
        )
        .init();
    let cli = Cli::parse();

    let cfg = mirsad::config::AppConfig::load(&cli.config)
        .with_context(|| format!("loading {}", cli.config.display()))?;
    let policy = policy::PolicyFile::load(std::path::Path::new(&cfg.policy.questions_file))
        .context("loading decision-questions file")?;
    // The configured question ids must exist in the policy file.
    for q in [
        &cfg.policy.spam_question,
        &cfg.policy.language_question,
        &cfg.policy.category_question,
    ] {
        anyhow::ensure!(
            policy.questions.contains_key(q),
            "policy question '{q}' missing from {}",
            cfg.policy.questions_file
        );
    }
    tracing::info!(
        "policy loaded: {} questions from {}",
        policy.questions.len(),
        cfg.policy.questions_file
    );

    if cli.check_config {
        println!("config OK: {}", cli.config.display());
        println!("questions OK: {}", cfg.policy.questions_file);
        return Ok(());
    }

    let secrets = Secrets::from_env();
    let api_key = secrets
        .openrouter_api_key
        .clone()
        .filter(|s| !s.is_empty())
        .context("set OPENROUTER_API_KEY in the environment")?;
    if cfg.matrix.user_id.is_empty() {
        anyhow::bail!("matrix.user_id must be set in config.toml");
    }

    let decider = DeciderClient::new(
        cfg.decider.base_url.clone(),
        cfg.decider.model.clone(),
        api_key,
        cfg.decider.app_title.clone(),
        cfg.decider.http_referer.clone(),
        cfg.decider.timeout_secs,
    )?;
    let store = Store::open(&cfg.storage.sqlite_path)?;
    let state = Arc::new(matrix::AppState {
        config: cfg.clone(),
        questions: policy.as_api_value(),
        spam_q: cfg.policy.spam_question.clone(),
        lang_q: cfg.policy.language_question.clone(),
        cat_q: cfg.policy.category_question.clone(),
        decider,
        store,
        reports: Mutex::new(HashMap::new()),
        history_cache: Mutex::new(HashMap::new()),
        live: std::sync::atomic::AtomicBool::new(false),
    });

    let client = matrix::login(&cfg, &secrets, &state.store).await?;
    matrix::run(client, state).await
}
