# Mirsad (مرصاد)

Autonomous AI spam moderation bot for the Aosus community's Matrix rooms (Telegram groups are covered through the Matrix bridge).

Mirsad watches configured rooms, asks an inexpensive **decision model** (via the **OpenRouter Decisions API**, default `perplexity/pplx-decider-v1.2`) whether each message from an untrusted sender is spam, and enforces the result: newcomers posting clear spam are removed and banned; established members are never auto-banned. Any member can reply `/spam` (or mention the bot) to trigger a fresh classification with a public diagnostic.

## How it works

- **Matrix-first.** One Matrix bot account moderates every bridged platform. Bans/redactions propagate through the bridge to Telegram.
- **Prompt-defined behaviour.** The spam criteria live in `config/decision-questions.json`, sent verbatim as the Decisions-API `questions` with the message as `state`. The Rust code only thresholds the returned probabilities (`spam ≥ 0.9` → enforce). Retuning = edit JSON + restart.
- **Two independent trust axes.**
  - *Activity trust* (routine scanning on/off): ≥10 legit messages across ≥3 distinct days (configurable). Trusted-but-inactive-90-days resumes scanning.
  - *Membership protection* (ban safety): only verified newcomers (join observed live after startup) can be auto-banned. Everyone else — baseline members, unknown history — is protected from automatic permanent bans forever.
- **Exclusions.** `exclusions.user_ids` (exact or `prefix*`) skips admins/bots from scanning and punishment entirely.
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
