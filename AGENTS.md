# Nuvi — Agent Guide

## Project facts

- **Unreleased app.** Never build backward-compatibility shims, deprecation windows, or migration paths for apps

## Making changes

- Pull latest main before starting; if there are conflicts, STOP.
- If product or architectural intent is unclear, ask — don't guess.
- Create a branch before committing; never commit to main or a release branch.
- Tests are expensive to write and maintain. Only add or expand tests for
  high-value critical behavior, serious regression risk, or contracts that
  would be costly to break. Keep tests focused; avoid low-signal harnesses.

## Communication

- Keep all written content as short as possible without omitting necessary
  detail. Expand only when explicitly asked.

## ⚠️ Critical restrictions

- **NEVER EVER merge a pull request.** This prohibition is absolute, even if
  the pull request is approved, checks pass, or the user asks you to ship it.
- **NEVER run the application ** without explicit permission. Tests, typechecks, and builds are fine.

