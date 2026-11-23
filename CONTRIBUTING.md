# Contributing

Thanks for helping. This is a monorepo: `backend/` (Rust), `frontend/` (Next.js), `dev/` (test harness), `deploy/` (proxy config).

## Setup
```bash
cp backend/.env.example backend/.env && cp frontend/.env.example frontend/.env.local   # fill in Clerk + Gemini keys
make backend      # terminal 1
make frontend     # terminal 2
```

## Before opening a pull request
```bash
make test lint    # unit tests, type check, lint
make e2e          # API end-to-end suite (fake Clerk + fake LLM, no accounts needed)
```
CI runs the same checks plus the Docker builds.

## Guidelines
- Keep pull requests focused, and describe the *why*.
- Changes to `backend/knowledge/`, `backend/src/safety.rs` or the prompts in `agent.rs` change medical behaviour: say so in the PR and get a clinician's review.
- Never commit `.env` files, keys or real health data.
