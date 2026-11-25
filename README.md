# Mirsad (مرصاد)

Autonomous AI spam moderation bot for the Aosus community's Matrix rooms (Telegram groups are covered through the Matrix bridge).

Mirsad watches configured rooms, asks an inexpensive **decision model** (via the **OpenRouter Decisions API**, default `perplexity/pplx-decider-v1.2`) whether each message from an untrusted sender is spam, and enforces the result: newcomers posting clear spam are removed and banned; established members are never auto-banned. Any member can reply `/spam` (or mention the bot) to trigger a fresh classification with a public diagnostic.

## How it works

- **Matrix-first.** One Matrix bot account moderates every bridged platform. Bans/redactions propagate through the bridge to Telegram.
- **Prompt-defined behaviour.** The spam criteria live in `config/decision-questions.json`, sent verbatim as the Decisions-API `questions` with the message as `state`. The Rust code only thresholds the returned probabilities (`spam ≥ 0.9` → enforce). Retuning = edit JSON + restart.
- **Two independent trust axes.**
  - *Activity trust* (routine scanning on/off): ≥10 legit messages across ≥3 distinct days (configurable). If the sender's visible room history already meets that bar, trust is granted immediately (fast-track) instead of waiting for prospective counting. Trusted-but-inactive-90-days resumes scanning.
  - *Membership protection* (ban safety): only verified newcomers (join observed after startup, or no visible history at all) can be auto-banned. Everyone else — baseline members, unknown history, or established activity — is protected from automatic permanent bans forever.
- **Exclusions.** `exclusions.user_ids` (exact or `prefix*`) skips admins/bots/bridges, and room staff (moderators/admins/owners by Matrix power level, default ≥50) are skipped automatically — no listing needed.
- **Rose stays authoritative** for Telegram warnings: Mirsad sends the configured warn command through the bridge and keeps no Telegram counters. Matrix-native warnings are counted locally for escalation.
- **SQLite only.** Activity counters, trust/protection flags, enforcement ids (duplicate suppression), Matrix-native warns. No message history, no classification cache — every `/spam` report re-runs the model.

## Quick start

```bash
cp config/config.example.toml config/config.toml
cp config/decision-questions.example.json config/decision-questions.json
cp .env.example .env   # then fill in MATRIX_PASSWORD + OPENROUTER_API_KEY
# 1. Edit config.toml (rooms, thresholds, exclusions).
# 2. Tune config/decision-questions.json (the actual spam policy).
# 3. Start in observation mode first (dry_run = true), then go live.
docker compose up --build -d
docker compose logs -f
```

Validate configs without connecting anywhere:

```bash
cargo run -- --config config/config.toml --check-config
```

Without Docker:

```bash
export MATRIX_PASSWORD=... OPENROUTER_API_KEY=sk-or-v1-...
cargo run --release -- --config config/config.toml
```

## Configuration

| File | Purpose | Rebuild to change? |
|---|---|---|
| `config/config.toml` | rooms, model, thresholds, trust, exclusions, Rose, storage | No — restart only |
| `config/decision-questions.json` | the actual classification policy (spam/language/category questions) | No — restart only |
| env (`MATRIX_PASSWORD`, `OPENROUTER_API_KEY`) | secrets | No |

Key knobs: `policy.spam_threshold` / `language_threshold` (default 0.9), `policy.dry_run` (observation mode), `trust.*`, `exclusions.user_ids`, `rose.warn_template` (`{user}` = Telegram numeric id for bridged users, `{reason}` auto-filled).

Switch models with one line (`decider.model`), e.g. `perplexity/pplx-decider-flash-v1.2` for half the input price. Note Flash answers each question independently (billed per question); v1.2 groups questions (state billed once) — see [Grouping questions](https://docs.perplexity.ai/docs/decisions/quickstart).

## Manual reports

Reply to any suspicious message with `/spam` or a mention of the bot. Mirsad re-classifies from scratch and posts a public report: scores, thresholds, category probabilities, trust/protection status, action taken, and why. Repeat reports on the same message never double-punish (rate-limited per reporter, duplicate enforcement suppressed).

## Local benchmark dataset (opt-in)

Add `[benchmark]` with `directory = "data/benchmark"` and `rooms = ["!community:example.org"]` to capture only the listed monitored rooms. The parent directory must already exist and be trusted. Mirsad creates the benchmark directory and SQLite file with private permissions. Leave `directory` unset to disable capture. This feature does not change what Mirsad scans, classifies, or enforces; it works in dry-run mode too.

The dataset contains message text sent to the model and Matrix identifiers. It does not store image bytes, image URLs from attachments, or API error bodies. Keep the database, backups, and exports local and access-restricted. Mirsad does not fetch historical data for the benchmark. There is no automatic expiration: set a retention schedule, disable capture to stop collection, and delete the database and exports when no longer needed. Use a separate directory for a new collection if its room scope changes; changing the allowlist does not purge or filter old records.

```sh
mirsad --config config/config.toml benchmark-feedback \
  --room '!community:example.org' --event '$message-id' --label false-positive
mirsad --config config/config.toml benchmark-export ./benchmark.jsonl
```

These commands need local database access, not Matrix or model credentials. Restrict database access to the bot service account and trusted moderator operators; Mirsad does not check Matrix permissions for local commands. Export requires a new output path ending in `.jsonl` and creates a private file. Git and Docker ignore that suffix by default. SQLite uses WAL so capture can continue during export; keep the database and its `-wal` and `-shm` sidecars together in the private directory. Labels are `spam`, `not-spam`, `false-positive` (benign), and `false-negative` (spam). An explicit label can reference an event that was never analyzed; no text is fetched for it. Feedback is append-only, so resolve conflicting judgments before scoring.

JSONL records carry a `schema_version`, `kind`, `room_id`, `recorded_at`, and `data`:

- `analysis`: one completed classifier attempt, with target event, text state, image-submitted flag, model, questions, and answers or failure status. Group by room and target event to compare models; image-assisted samples cannot be replayed from JSONL alone.
- `manual_report`: one accepted report, deduplicated by report event ID. Distinct reports of the same target are separate observations; join a report-triggered analysis by `report_event_id`. Reports are not labels.
- `external_ban`: a ban membership event sent by an actor other than Mirsad (human or other automation). It identifies a user, never a message, and is not a label.
- `moderator_feedback`: an explicit local human judgment for a target event; this is the only ground-truth source.

## Deployment checklist (do this before `dry_run = false`)

- [ ] Run 1–2 weeks with `dry_run = true`; review logs + mod-room flags.
- [ ] Verify Matrix bans/redactions propagate to Telegram via your bridge.
- [ ] Verify Telegram replies/mentions reach the bot, and the Rose warn command shape works from Matrix (adjust `rose.warn_template`).
- [ ] Confirm `/spam` reports produce public diagnostics in-room.
- [ ] Confirm excluded admins are never touched.
- [ ] Restart the container and confirm trust state survived (`/data` volume).

## Development

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets        # unit + integration (mocked HTTP, temp SQLite)
```

CI (`.github/workflows/ci.yml`) runs fmt, clippy, all tests, a release build, a config smoke test, and a Docker build on every push/PR. `main` should stay green; treat red CI as a merge blocker.

## License

MIT — see [LICENSE](LICENSE).
