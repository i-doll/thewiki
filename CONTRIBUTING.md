# Contributing to thewiki

Thanks for your interest. thewiki is in pre-alpha — design discussions and issue triage are the main contribution surface right now.

## How to help (pre-alpha)

- **Comment on issues** in the [M0 milestone](https://github.com/i-doll/thewiki/milestones) if you have strong opinions about architecture or feature scope.
- **File issues** for concerns about the design captured in the roadmap.
- **Reach out** if you want to take on a specific issue — open a comment claiming it so we don't duplicate work.

## How to help (once code exists)

This section will be filled out at the end of M0, when there's a real codebase to contribute to. Expected workflow:

- Fork → branch (`feat/…`, `fix/…`, `chore/…`) → PR against `main`.
- New features need tests; bug fixes need regression tests.
- Sign your commits (DCO).
- One logical change per PR.

### Required CI checks

Every PR must keep these green before merge. Run them locally with the same
commands the CI runs in `.github/workflows/ci.yml`:

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-features`
- `cargo deny check` (advisories, licenses, bans, sources)
- For changes under `web/`: `pnpm install --frozen-lockfile`, then `pnpm lint`,
  `pnpm typecheck`, and `pnpm build`.

A Playwright smoke job runs on PRs labelled `area:frontend`. It is a
placeholder today; real end-to-end coverage will land later in M0 / M1.

## Code of Conduct

We follow the [Contributor Covenant 2.1](./CODE_OF_CONDUCT.md). Be excellent to each other.

## License

By contributing, you agree that your contributions will be licensed under [AGPL-3.0](./LICENSE).
