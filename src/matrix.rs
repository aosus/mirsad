//! Matrix bot runtime: sync loop, message scanning, /spam reports, enforcement.
//!
//! Design notes:
//! - Routine scanning is skipped for excluded users and trusted-active users.
//! - Every `/spam` report triggers a FRESH classification regardless of trust.
//! - Duplicate punishments are suppressed via the SQLite enforcement table,
//!   but diagnostics are always replied publicly.
//! - Classification failures never punish; they are logged + optionally
//!   notified to the mod room.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use matrix_sdk::{
    authentication::matrix::MatrixSession,
    config::SyncSettings,
    event_handler::Ctx,
    room::MessagesOptions,
    ruma::{
        events::room::{
            member::{MembershipState, OriginalSyncRoomMemberEvent, StrippedRoomMemberEvent},
            message::{
                MessageType, OriginalSyncRoomMessageEvent, Relation, RoomMessageEventContent,
            },
        },
        OwnedEventId, OwnedUserId, UInt,
    },
    store::RoomLoadSettings,
    Client, Room, RoomState,
};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use crate::{
    config::{AppConfig, Secrets},
    decider::{message_state, Classification, DeciderClient},
    enforcement::{decide, Action, EnforcementInput},
    history::{check_sender_history, decide_verdict, history_label, HistoryInfo, HistoryVerdict},
    report::diagnostic_message,
    rose::{build_warn_command, is_bridged_telegram, spam_reason},
    store::Store,
};

pub struct AppState {
    pub config: AppConfig,
    pub questions: serde_json::Value,
    pub spam_q: String,
    pub lang_q: String,
    pub cat_q: String,
    pub decider: DeciderClient,
    pub store: Store,
    pub reports: Mutex<HashMap<String, Vec<Instant>>>,
    /// Suspect history verdicts per room|user, with check timestamp (24h TTL).
    pub history_cache: Mutex<HashMap<String, (i64, HistoryInfo)>>,
    pub live: AtomicBool,
}

impl AppState {
    pub fn trust_labels(&self, user_id: &str) -> (String, String) {
        let row = self.store.get_user(user_id).unwrap_or_default();
        // Need a UserRow default — construct labels defensively.
        let _ = row;
        self.trust_labels_inner(user_id)
    }

    fn trust_labels_inner(&self, user_id: &str) -> (String, String) {
        match self.store.get_user(user_id) {
            Ok(u) => {
                let trust = if u.trusted {
                    "trusted"
                } else if u.activity_count == 0 {
                    "untrusted newcomer"
                } else {
                    "untrusted (building activity)"
                };
                let prot = if u.protected {
                    if u.baseline_seen {
                        "protected (established member)"
                    } else {
                        "protected (unknown history — conservative)"
                    }
                } else {
                    "verified newcomer"
                };
                (trust.to_string(), prot.to_string())
            }
            Err(_) => (
                "unknown".to_string(),
                "protected (lookup failed)".to_string(),
            ),
        }
    }
}

/// Build a logged-in client.
///
/// Prefers restoring a previously saved session (zero `/login` requests, so
/// restarts never burn Synapse's per-IP login rate-limit budget). Falls back
/// to username+password login and persists the new session for next time.
pub async fn login(config: &AppConfig, secrets: &Secrets, store: &Store) -> Result<Client> {
    let builder = Client::builder().homeserver_url(&config.matrix.homeserver);
    let builder = builder.sqlite_store(&config.storage.matrix_store_path, None);
    let client = builder.build().await.context("building Matrix client")?;
    if let Ok(Some(saved)) = store.meta_get(SESSION_KEY) {
        match serde_json::from_str::<MatrixSession>(&saved) {
            Ok(session) => {
                match client
                    .matrix_auth()
                    .restore_session(session, RoomLoadSettings::default())
                    .await
                {
                    Ok(()) => {
                        info!("restored saved Matrix session (no fresh login needed)");
                        return Ok(client);
                    }
                    Err(e) => {
                        warn!("saved Matrix session invalid ({e:#}); logging in fresh");
                    }
                }
            }
            Err(e) => warn!("could not parse saved Matrix session ({e:#}); logging in fresh"),
        }
    }
    let username = config
        .matrix
        .user_id
        .trim_start_matches('@')
        .split(':')
        .next()
        .unwrap_or(&config.matrix.user_id);
    if let Some(password) = secrets.matrix_password.as_deref() {
        client
            .matrix_auth()
            .login_username(username, password)
            .device_id(&config.matrix.device_id)
            .initial_device_display_name(&config.matrix.device_name)
            .send()
            .await
            .context("Matrix login failed")?;
        info!("logged in as {}", config.matrix.user_id);
        // Persist the session (access token) so the next boot restores it.
        match client.matrix_auth().session() {
            Some(session) => match serde_json::to_string(&session) {
                Ok(json) => {
                    if let Err(e) = store.meta_set(SESSION_KEY, &json) {
                        warn!("could not persist Matrix session: {e:#}");
                    }
                }
                Err(e) => warn!("could not serialize Matrix session: {e:#}"),
            },
            None => warn!("login succeeded but no session available to persist"),
        }
    } else if secrets.matrix_access_token.is_some() {
        anyhow::bail!(
            "MATRIX_ACCESS_TOKEN restore is not implemented in v1; set MATRIX_PASSWORD instead"
        );
    } else {
        anyhow::bail!("set MATRIX_PASSWORD (Matrix bot password) in the environment");
    }
    Ok(client)
}

const SESSION_KEY: &str = "matrix_session";

pub async fn run(client: Client, state: Arc<AppState>) -> Result<()> {
    // Greet + baseline: first sync establishes the token; handlers attach after
    // so historical messages are never scanned or punished.
    let sync_once = client.sync_once(SyncSettings::default()).await?;
    mark_startup_baseline(&state);

    if state.config.matrix.autojoin {
        client.add_event_handler(handle_invite);
    }
    client.add_event_handler(handle_member);
    {
        let s = state.clone();
        client.add_event_handler(
            move |ev: OriginalSyncRoomMessageEvent, room: Room, client: Client| {
                let s = s.clone();
                async move { on_message(ev, room, client, s).await }
            },
        );
    }
    // Share state with member handler via context.
    client.add_event_handler_context(state.clone());
    state.live.store(true, Ordering::SeqCst);

    info!("starting sync loop");
    let settings = SyncSettings::default().token(sync_once.next_batch);
    tokio::select! {
        r = client.sync(settings) => {
            r.context("Matrix sync failed")?;
        }
        _ = tokio::signal::ctrl_c() => {
            info!("shutdown requested");
        }
    }
    Ok(())
}

fn mark_startup_baseline(state: &Arc<AppState>) {
    // First-ever run records the startup timestamp. Users first observed
    // within the grace window count as baseline (present at deployment).
    if let Ok(None) = state.store.meta_get("first_start_ts") {
        let now = chrono::Utc::now().timestamp().to_string();
        let _ = state.store.meta_set("first_start_ts", &now);
        info!("first startup recorded; 24h baseline grace window active");
    }
}

/// A user observed for the first time inside the 24h post-deploy window is
/// treated as baseline (established), otherwise unknown → protected.
fn note_first_sighting(state: &AppState, user_id: &str) {
    let row = state.store.get_user(user_id).unwrap_or_else(|_| {
        // unreachable with file-backed store; keep conservative
        crate::store::UserRow {
            user_id: user_id.to_string(),
            ..Default::default()
        }
    });
    let is_new = row.activity_count == 0
        && !row.baseline_seen
        && !row.verified_newcomer
        && row.last_activity_ts == 0;
    if !is_new {
        return;
    }
    let first: i64 = state
        .store
        .meta_get("first_start_ts")
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let now = chrono::Utc::now().timestamp();
    if first > 0 && now.saturating_sub(first) < 86_400 {
        let _ = state.store.mark_membership(user_id, false, true);
    }
    // Outside the window: stays protected-unknown until a join event verifies it.
}

async fn handle_invite(ev: StrippedRoomMemberEvent, client: Client, room: Room) {
    if client.user_id().is_none_or(|u| *u != ev.state_key) {
        return;
    }
    info!("auto-joining {}", room.room_id());
    let mut delay = 2u64;
    loop {
        match room.join().await {
            Ok(_) => {
                info!("joined {}", room.room_id());
                break;
            }
            Err(e) => {
                warn!("join {} failed ({e}); retry in {delay}s", room.room_id());
                tokio::time::sleep(Duration::from_secs(delay)).await;
                delay = (delay * 2).min(3600);
            }
        }
    }
}

async fn handle_member(ev: OriginalSyncRoomMemberEvent, _room: Room, ctx: Ctx<Arc<AppState>>) {
    // A live join observed after startup marks the user a verified newcomer —
    // but only if they have no recorded activity and are not baseline.
    // (Handlers attach after the initial sync, so backfill never reaches here.)
    if ev.content.membership == MembershipState::Join {
        let uid = ev.state_key.to_string();
        if ctx.live.load(Ordering::SeqCst) {
            let fresh = ctx
                .store
                .get_user(&uid)
                .map(|u| u.activity_count == 0 && !u.baseline_seen && !u.verified_newcomer)
                .unwrap_or(true);
            if fresh {
                let _ = ctx.store.mark_membership(&uid, true, false);
            }
        }
    }
}

async fn on_message(
    ev: OriginalSyncRoomMessageEvent,
    room: Room,
    client: Client,
    state: Arc<AppState>,
) {
    if room.state() != RoomState::Joined {
        return;
    }
    let room_id = room.room_id().to_string();
    if !state.config.room_monitored(&room_id) {
        return;
    }
    let own_id = client.user_id().map(|u| u.to_string()).unwrap_or_default();
    let sender = ev.sender.to_string();
    if sender == own_id {
        return;
    }
    let MessageType::Text(text) = ev.content.msgtype.clone() else {
        return;
    };
    let body = text.body.trim().to_string();
    if body.is_empty() {
        return;
    }

    if is_report(&ev.content, &body, &own_id) {
        handle_report(ev, room, client, state, body).await;
        return;
    }

    // Routine scan path.
    if state.config.is_excluded(&sender) {
        return;
    }
    note_first_sighting(&state, &sender);
    // Replies to our own messages (reports, warnings, notices) are conversation
    // about moderation, not new community content: never analyze them. Without
    // this, discussing a diagnostic would classify our own quoted spam text.
    if let Some(parent_id) = reply_target_id(&ev.content) {
        if let Ok(parent_sender) = fetch_event_sender(&room, &parent_id).await {
            if !own_id.is_empty() && parent_sender == own_id {
                return;
            }
        }
        // Fetch failed: fall through to normal scanning (fail-closed for spam).
    }
    let now = chrono::Utc::now().timestamp();
    let needs = state
        .store
        .needs_scan(&sender, now, state.config.trust.inactivity_days)
        .unwrap_or(true);
    if !needs {
        return;
    }

    match classify_message(&state, &body, &sender, &room_id).await {
        Ok(cls) => {
            let row = state
                .store
                .get_user(&sender)
                .unwrap_or_else(|_| crate::store::UserRow {
                    user_id: sender.clone(),
                    ..Default::default()
                });
            let excluded = state.config.is_excluded(&sender);
            let spam_hit = cls.spam_score >= state.config.policy.spam_threshold;
            let (effective_protected, _history) = resolve_protection(
                &state,
                &room,
                &sender,
                row.protected,
                row.baseline_seen,
                row.verified_newcomer,
                spam_hit,
            )
            .await;
            let decision = decide(EnforcementInput {
                cls: &cls,
                spam_threshold: state.config.policy.spam_threshold,
                language_threshold: state.config.policy.language_threshold,
                language_action: &state.config.policy.language_action,
                protected: effective_protected,
                excluded,
                dry_run: state.config.policy.dry_run,
                prior_warns: row.matrix_warns,
            });
            // Legitimate messages build activity trust (filler excluded).
            if decision.action == Action::None
                && !cls_spamish(&cls, state.config.policy.spam_threshold)
                && body.chars().count() >= state.config.trust.filler_min_chars
            {
                let _ = state.store.record_legit_activity(
                    &sender,
                    now,
                    state.config.trust.min_messages,
                    state.config.trust.min_days,
                );
            }
            apply_enforcement(
                &state,
                &room,
                &client,
                ev.event_id.as_str(),
                &sender,
                &cls,
                &decision,
            )
            .await;
        }
        Err(e) => {
            // Never punish on classifier failure.
            warn!("classification failed for {}: {e:#}", ev.event_id);
            notify_mods(
                &state,
                &client,
                &format!("Classifier error in {room_id}: {e:#}"),
            )
            .await;
        }
    }
}

fn cls_spamish(cls: &Classification, threshold: f64) -> bool {
    cls.spam_score >= threshold * 0.5
}

/// A message is a manual spam report when it starts with `/spam` or mentions
/// the bot. Works from Matrix and (via the bridge) Telegram.
fn is_report(content: &RoomMessageEventContent, body: &str, own_id: &str) -> bool {
    if body.starts_with("/spam") {
        return true;
    }
    if !own_id.is_empty()
        && content
            .mentions
            .as_ref()
            .is_some_and(|m| m.user_ids.iter().any(|u| u.as_str() == own_id))
    {
        return true;
    }
    // Fallback for bridged clients that don't set structured mentions:
    // a literal @localpart mention in the body.
    if !own_id.is_empty() {
        if let Some(local) = own_id.strip_prefix('@').and_then(|s| s.split(':').next()) {
            if body.contains(&format!("@{local}")) {
                return true;
            }
        }
    }
    false
}

async fn handle_report(
    ev: OriginalSyncRoomMessageEvent,
    room: Room,
    client: Client,
    state: Arc<AppState>,
    body: String,
) {
    let reporter = ev.sender.to_string();
    // Rate limit.
    if !check_rate_limit(&state, &reporter).await {
        let _ = room
            .send(
                matrix_sdk::ruma::events::room::message::RoomMessageEventContent::text_plain(
                    "Rate limit: please wait a few minutes before reporting again.",
                ),
            )
            .await;
        return;
    }
    // Find the reported event: prefer in-reply-to, else the message itself is unusable.
    let reported_id: Option<String> = reply_target_id(&ev.content);
    let Some(target_id) = reported_id else {
        let _ = room.send(matrix_sdk::ruma::events::room::message::RoomMessageEventContent::text_plain(
            "Reply to the suspicious message with /spam (or mention me in the reply) so I know what to check.",
        )).await;
        return;
    };
    let _ = body;
    // Fetch the reported event's body + author.
    let (target_body, target_author) = match fetch_event_text(&room, &target_id).await {
        Ok(v) => v,
        Err(e) => {
            let _ = room
                .send(
                    matrix_sdk::ruma::events::room::message::RoomMessageEventContent::text_plain(
                        format!("Could not retrieve the reported message ({target_id}): {e:#}"),
                    ),
                )
                .await;
            return;
        }
    };

    // Never analyze our own messages: a reply to one of our reports, warnings
    // or diagnostics would otherwise make us classify our own output (and the
    // diagnostic text quotes the original spam, guaranteeing nonsense).
    let own_id = client.user_id().map(|u| u.to_string()).unwrap_or_default();
    if !own_id.is_empty() && target_author == own_id {
        let _ = room
            .send(
                matrix_sdk::ruma::events::room::message::RoomMessageEventContent::text_plain(
                    crate::report::own_message_response(),
                ),
            )
            .await;
        return;
    }

    // FRESH classification, ignoring trust/cache.
    let room_id = room.room_id().to_string();
    let cls = match classify_message(&state, &target_body, &target_author, &room_id).await {
        Ok(c) => c,
        Err(e) => {
            let _ = room.send(matrix_sdk::ruma::events::room::message::RoomMessageEventContent::text_plain(
                format!("Classification failed, no action taken. The model/provider may be unavailable: {e:#}"),
            )).await;
            return;
        }
    };
    let row = state
        .store
        .get_user(&target_author)
        .unwrap_or_else(|_| crate::store::UserRow {
            user_id: target_author.clone(),
            ..Default::default()
        });
    let spam_hit = cls.spam_score >= state.config.policy.spam_threshold;
    let (effective_protected, history_info) = resolve_protection(
        &state,
        &room,
        &target_author,
        row.protected,
        row.baseline_seen,
        row.verified_newcomer,
        spam_hit,
    )
    .await;
    let decision = decide(EnforcementInput {
        cls: &cls,
        spam_threshold: state.config.policy.spam_threshold,
        language_threshold: state.config.policy.language_threshold,
        language_action: &state.config.policy.language_action,
        protected: effective_protected,
        excluded: state.config.is_excluded(&target_author),
        dry_run: state.config.policy.dry_run,
        prior_warns: row.matrix_warns,
    });
    let duplicate = state.store.already_enforced(&target_id).unwrap_or(false);
    let mut failures = Vec::new();
    if !duplicate {
        failures = apply_enforcement(
            &state,
            &room,
            &client,
            &target_id,
            &target_author,
            &cls,
            &decision,
        )
        .await;
    }
    let (trust_label, prot_label) = state.trust_labels_inner(&target_author);
    let prot_label = if !effective_protected && row.protected {
        "newcomer (insufficient room history)".to_string()
    } else {
        prot_label
    };
    let outcome = if duplicate || failures.is_empty() {
        None
    } else {
        Some(failures.join("; "))
    };
    let msg = diagnostic_message(
        &target_body,
        &target_id,
        &target_author,
        &cls,
        state.config.policy.spam_threshold,
        state.config.policy.language_threshold,
        &trust_label,
        &prot_label,
        &history_label(&history_info),
        &decision,
        duplicate,
        outcome.as_deref(),
    );
    let _ = room
        .send(matrix_sdk::ruma::events::room::message::RoomMessageEventContent::text_plain(msg))
        .await;
}

/// Resolve effective ban-protection for a spam-flagged sender.
///
/// Verified newcomers are ban-eligible; baseline/established members stay
/// protected. For unknown-history senders, one server-filtered history lookup
/// decides: no visible prior events → newcomer; anything found, a disabled
/// check, or any lookup failure → protected. Verdicts cache per room+sender
/// for 24h so repeat offenders cost one lookup a day.
async fn resolve_protection(
    state: &AppState,
    room: &Room,
    sender: &str,
    row_protected: bool,
    baseline_seen: bool,
    verified_newcomer: bool,
    spam_hit: bool,
) -> (bool, HistoryInfo) {
    if !spam_hit || !row_protected || baseline_seen || verified_newcomer {
        return (row_protected, HistoryInfo::default());
    }
    if !state.config.history.enabled {
        return (true, HistoryInfo::default());
    }
    let key = format!("{}|{sender}", room.room_id());
    if let Some(info) = get_cached_history(state, &key).await {
        return (
            matches!(info.verdict, Some(HistoryVerdict::Established)),
            info,
        );
    }
    let info = match check_sender_history(room, sender).await {
        Ok(info) => info,
        Err(e) => {
            warn!("sender history check failed for {sender}: {e:#}; staying protected");
            return (true, HistoryInfo::default());
        }
    };
    let verdict = decide_verdict(
        info.prior_events,
        info.distinct_days,
        state.config.trust.min_messages,
        state.config.trust.min_days,
    );
    let info = HistoryInfo {
        verdict: Some(verdict),
        ..info
    };
    state
        .history_cache
        .lock()
        .await
        .insert(key, (chrono::Utc::now().timestamp(), info.clone()));
    (matches!(verdict, HistoryVerdict::Established), info)
}

async fn get_cached_history(state: &AppState, key: &str) -> Option<HistoryInfo> {
    let cache = state.history_cache.lock().await;
    let (ts, info) = cache.get(key)?;
    if chrono::Utc::now().timestamp() - ts < 86_400 {
        Some(info.clone())
    } else {
        None
    }
}

async fn check_rate_limit(state: &AppState, reporter: &str) -> bool {
    let mut map = state.reports.lock().await;
    let now = Instant::now();
    let window = Duration::from_secs(5 * 60);
    let entry = map.entry(reporter.to_string()).or_default();
    entry.retain(|t| now.duration_since(*t) < window);
    if entry.len() >= state.config.report.max_reports_per_5min {
        return false;
    }
    entry.push(now);
    true
}

async fn classify_message(
    state: &AppState,
    body: &str,
    author: &str,
    room_id: &str,
) -> Result<Classification> {
    let st = message_state(body, author, room_id, state.config.decider.max_chars);
    let resp = state
        .decider
        .classify(st.clone(), state.questions.clone())
        .await?;
    let raw = serde_json::json!({"answers": resp.answers, "model": resp.model});
    Ok(Classification::from_response(
        &resp,
        raw,
        &state.spam_q,
        &state.lang_q,
        &state.cat_q,
        state.decider.model(),
    ))
}

/// The event id this message replies to, if any.
fn reply_target_id(content: &RoomMessageEventContent) -> Option<String> {
    content.relates_to.as_ref().and_then(|r| match r {
        Relation::Reply(reply) => Some(reply.in_reply_to.event_id.to_string()),
        _ => None,
    })
}

async fn fetch_event_sender(room: &Room, event_id: &str) -> Result<String> {
    let owned: OwnedEventId = event_id
        .try_into()
        .map_err(|e| anyhow::anyhow!("bad event id: {e}"))?;
    let tl = room
        .event(&owned, None)
        .await
        .context("fetching parent event")?;
    let v = serde_json::to_value(tl.kind.raw()).context("serializing event")?;
    Ok(v.get("sender")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string())
}

async fn fetch_event_text(room: &Room, event_id: &str) -> Result<(String, String)> {
    let owned: OwnedEventId = event_id
        .try_into()
        .map_err(|e| anyhow::anyhow!("bad event id: {e}"))?;
    let tl = room
        .event(&owned, None)
        .await
        .context("fetching reported event")?;
    let v = serde_json::to_value(tl.kind.raw()).context("serializing event")?;
    let body = v
        .pointer("/content/body")
        .and_then(|b| b.as_str())
        .unwrap_or("")
        .to_string();
    let sender = v
        .get("sender")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    if body.is_empty() {
        anyhow::bail!("reported event has no readable text body");
    }
    Ok((body, sender))
}

/// Apply the enforcement decision. Returns a list of human-readable failure
/// notes (empty when everything worked) so diagnostics report the OUTCOME,
/// not just the intent — a 403 on redact must never again look like a delete.
async fn apply_enforcement(
    state: &AppState,
    room: &Room,
    client: &Client,
    event_id: &str,
    author: &str,
    cls: &Classification,
    decision: &crate::enforcement::EnforcementDecision,
) -> Vec<String> {
    let mut failures = Vec::new();
    let room_id = room.room_id().to_string();
    match decision.action {
        Action::None => {}
        Action::NotifyMods => {
            notify_mods(
                state,
                client,
                &format!(
                    "Mirsad flag in {room_id}: {author} — {} (spam {:.1}%, lang {:.1}%, cat {})",
                    decision.explanation,
                    cls.spam_score * 100.0,
                    cls.language_score * 100.0,
                    cls.category
                ),
            )
            .await;
        }
        Action::Delete | Action::DeleteWarn | Action::DeleteBan => {
            if state
                .store
                .check_and_mark_enforced(event_id, &room_id, author, decision.action.as_str())
                .unwrap_or(true)
            {
                info!("duplicate enforcement suppressed for {event_id}");
                return vec!["already enforced earlier; no duplicate action taken".to_string()];
            }
            // Redact first so the content disappears even if later steps fail.
            if let Err(e) = redact(room, event_id).await {
                error!("redact {event_id} failed: {e:#}");
                failures.push(format!("message NOT deleted: {e:#}"));
            }
            if matches!(decision.action, Action::DeleteWarn | Action::DeleteBan) {
                if let Some(f) = warn_user(state, client, room, author, cls).await {
                    failures.push(f);
                }
            }
            if decision.action == Action::DeleteBan {
                let (bulk_ok, bulk_failed) =
                    bulk_redact_user_messages(room, author, event_id).await;
                if bulk_ok + bulk_failed > 0 {
                    info!("bulk redact for {author}: {bulk_ok} deleted, {bulk_failed} failed");
                }
                if bulk_failed > 0 {
                    failures.push(format!(
                        "bulk cleanup incomplete: {bulk_ok} deleted, {bulk_failed} failed"
                    ));
                }
                if let Err(e) = ban(room, author, cls).await {
                    error!("ban {author} failed: {e:#}");
                    failures.push(format!("user NOT banned: {e:#}"));
                } else {
                    info!("banned {author} for spam in {room_id}");
                }
            }
        }
    }
    failures
}

async fn redact(room: &Room, event_id: &str) -> Result<()> {
    let owned: OwnedEventId = event_id
        .try_into()
        .map_err(|e| anyhow::anyhow!("bad event id: {e}"))?;
    room.redact(&owned, Some("spam — automated removal by Mirsad"), None)
        .await
        .context("redact failed")?;
    Ok(())
}

/// Upper bound on recent messages pulled for newcomer bulk cleanup.
const BULK_REDACT_LIMIT: u8 = 100;

/// Redact the author's recent messages (newest first, excluding the already
/// handled triggering event). Best-effort: returns (deleted, failed).
async fn bulk_redact_user_messages(
    room: &Room,
    author: &str,
    exclude_event_id: &str,
) -> (usize, usize) {
    let target: OwnedUserId = match author.try_into() {
        Ok(u) => u,
        Err(_) => return (0, 0),
    };
    let mut opts = MessagesOptions::backward();
    opts.limit = UInt::from(BULK_REDACT_LIMIT);
    opts.filter.senders = Some(vec![target]);
    let page = match room.messages(opts).await {
        Ok(p) => p,
        Err(e) => {
            warn!("bulk redact history fetch failed for {author}: {e:#}");
            return (0, 0);
        }
    };
    let values: Vec<serde_json::Value> = page
        .chunk
        .iter()
        .filter_map(|ev| serde_json::to_value(ev.kind.raw()).ok())
        .collect();
    let mut ok = 0usize;
    let mut failed = 0usize;
    for id in redact_candidates(&values, author, exclude_event_id) {
        match redact(room, &id).await {
            Ok(()) => ok += 1,
            Err(e) => {
                warn!("bulk redact {id} failed: {e:#}");
                failed += 1;
            }
        }
    }
    (ok, failed)
}

/// Event ids in `events` (raw event JSON) authored by `sender` of type
/// m.room.message, excluding `exclude`. Pure and unit-tested.
fn redact_candidates(events: &[serde_json::Value], sender: &str, exclude: &str) -> Vec<String> {
    events
        .iter()
        .filter_map(|v| {
            let s = v.get("sender")?.as_str()?;
            let id = v.get("event_id")?.as_str()?;
            let t = v.get("type")?.as_str()?;
            if s == sender && id != exclude && t == "m.room.message" {
                Some(id.to_string())
            } else {
                None
            }
        })
        .collect()
}

async fn ban(room: &Room, user_id: &str, cls: &Classification) -> Result<()> {
    let owned: OwnedUserId = user_id
        .try_into()
        .map_err(|e| anyhow::anyhow!("bad user id: {e}"))?;
    let reason = spam_reason(&cls.category, cls.spam_score);
    room.ban_user(&owned, Some(reason.as_str()))
        .await
        .context("ban failed")?;
    Ok(())
}

/// Warn the author. Returns a failure note when the warning could not be
/// delivered (Rose command send failure or bookkeeping error), so the
/// diagnostic reflects reality.
async fn warn_user(
    state: &AppState,
    client: &Client,
    room: &Room,
    author: &str,
    cls: &Classification,
) -> Option<String> {
    if is_bridged_telegram(author) {
        if state.config.rose.enabled {
            let reason = spam_reason(&cls.category, cls.spam_score);
            let cmd = build_warn_command(&state.config.rose.warn_template, author, &reason);
            if let Err(e) = room
                .send(
                    matrix_sdk::ruma::events::room::message::RoomMessageEventContent::text_plain(
                        cmd,
                    ),
                )
                .await
            {
                error!("rose warn command failed: {e:#}");
                return Some(format!("warn NOT delivered via Rose: {e:#}"));
            }
            return None;
        }
        notify_mods(
            state,
            client,
            &format!(
                "Rose disabled; Telegram user {author} would be warned: {}",
                cls.category
            ),
        )
        .await;
        return Some(
            "warn NOT delivered: Rose integration is disabled (moderators notified instead)"
                .to_string(),
        );
    }
    match state.store.add_matrix_warn(author) {
        Ok(n) => {
            let _ = room
                .send(
                    matrix_sdk::ruma::events::room::message::RoomMessageEventContent::text_plain(
                            format!("{author}: warning {n} — unsolicited promotion is not allowed here. Further violations may lead to restrictions or moderator review."),
                        ),
                    )
                .await;
            None
        }
        Err(e) => {
            error!("matrix warn bookkeeping failed: {e:#}");
            Some(format!("warn NOT recorded: {e:#}"))
        }
    }
}

async fn notify_mods(state: &AppState, client: &Client, text: &str) {
    let Some(mod_room) = state.config.matrix.mod_room.as_deref() else {
        return;
    };
    let Ok(room_id) = mod_room.try_into() as Result<&matrix_sdk::ruma::RoomId, _> else {
        warn!("bad mod room id {mod_room}; dropping notice: {text}");
        return;
    };
    if let Some(room) = client.get_room(room_id) {
        let _ = room
            .send(
                matrix_sdk::ruma::events::room::message::RoomMessageEventContent::text_plain(
                    text.to_string(),
                ),
            )
            .await;
    } else {
        warn!("mod room {mod_room} not found; dropping notice: {text}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_content(body: &str, relates_to: Option<serde_json::Value>) -> RoomMessageEventContent {
        let mut v = serde_json::json!({"msgtype": "m.text", "body": body});
        if let Some(r) = relates_to {
            v["m.relates_to"] = r;
        }
        serde_json::from_value(v).expect("test content parses")
    }

    #[test]
    fn reply_target_extracted() {
        let c = text_content(
            "look at this",
            Some(serde_json::json!({"m.in_reply_to": {"event_id": "$parent:example.org"}})),
        );
        assert_eq!(reply_target_id(&c), Some("$parent:example.org".to_string()));
        let plain = text_content("just chatting", None);
        assert_eq!(reply_target_id(&plain), None);
    }

    #[test]
    fn bulk_redact_picks_own_messages_only() {
        let ev = |sender: &str, id: &str, typ: &str| serde_json::json!({"sender": sender, "event_id": id, "type": typ});
        let events = vec![
            ev("@spam:x", "$1", "m.room.message"),
            ev("@spam:x", "$trigger", "m.room.message"),
            ev("@other:x", "$2", "m.room.message"),
            ev("@spam:x", "$3", "m.room.member"),
            serde_json::json!({"no": "sender"}),
        ];
        assert_eq!(
            redact_candidates(&events, "@spam:x", "$trigger"),
            vec!["$1".to_string()]
        );
        assert!(redact_candidates(&events, "@nobody:x", "$trigger").is_empty());
    }
}
