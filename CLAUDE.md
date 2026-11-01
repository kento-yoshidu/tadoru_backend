# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

The **backend** for `tadoru` — a chained one-question-at-a-time quiz app where
prior knowledge must be built up step by step before you can reach the answer.

- This repo lives at `c:\github\tadoru\backend\` and is its own git repository.
- The **frontend** is a *separate* git repository at `c:\github\tadoru\frontend\`
  (Vite + React + TanStack Query/Router; not created yet). The parent folder `c:\github\tadoru\` is just a container, not a repo.
- Full product spec (shared with the frontend): [docs/SPEC.md](docs/SPEC.md).
  Implementation plans: [docs/plans/](docs/plans/).

## Stack

- Web: `actix-web` 4
- DB: SurrealDB via the `surrealdb` crate — **no ORM**, raw SurrealQL strings.
  - Local dev: a `surreal` server started by `scripts/start-db.bat`
    (`surrealkv://data/dev.db`, bound to `127.0.0.1:8000`). `main.rs` connects over
    `ws://` using the `SURREAL_*` env vars (see `.env.example`).
  - `examples/db_demo.rs` uses the *embedded* engine (`kv-surrealkv`) instead — no
    server needed. Kept for learning/experiments.
  - Prod: Surreal Cloud over `Ws`, connection info from env vars only.
- Config: `dotenvy` loads `.env` in local dev; env vars used directly otherwise.
  Keys: `SURREAL_ENDPOINT`, `SURREAL_NS`, `SURREAL_DB`, `SURREAL_USER`,
  `SURREAL_PASS`, `PORT`.

## Data model (see docs/SPEC.md §2 for detail)

`unit` (theme) → `question`s → `choice`s. A `choice` may have a `leads_to` graph
edge (`RELATE`) to the next `question`; no edge = dead end / retry. Record links
(not edges) are used for the parent/child relations.

## API status

Implemented in `src/main.rs`:
- `GET /db-health`
- `GET /api/units` — unit list (never returns `is_correct`)

Planned, not yet implemented — see [docs/plans/question-answer-api.md](docs/plans/question-answer-api.md):
- `GET /api/units/:unit_id/start` — first question + choices of a unit
- `POST /api/questions/:question_id/answer` (`{ choice_id }`) — grade, return next question / completion

Response DTOs for the above already exist in `src/model.rs` (hence the current
dead-code warnings for `NextQuestionRow` / `CorrectnessRow`). CORS (`actix-cors`)
still needs to be added for the frontend dev server.

The current design drops the `attempt` concept from docs/SPEC.md (no progress
persistence / resume) — see SPEC §5, still undecided.

## Commands

- Build: `cargo build`
- Run: `cargo run` (needs the local DB server running first: `scripts\start-db.bat`)
- Check: `cargo check`
- Test: `cargo test` (no tests yet)
- DB demo (embedded, no server): `cargo run --example db_demo`

## Authoring quiz content

`.claude/skills/quiz-question/` — skill for adding connected quiz questions to
`docs/content/*.md` from a keyword or a screenshot of an exam question.
