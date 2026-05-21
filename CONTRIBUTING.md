# Contributing to thewiki

Thanks for your interest. thewiki is in pre-alpha — design discussions and issue triage are the main contribution surface right now.

## How to help (pre-alpha)

- **Comment on issues** in the [M0 milestone](https://github.com/i-doll/thewiki/milestones) if you have strong opinions about architecture or feature scope.
- **File issues** for concerns about the design captured in the roadmap.
- **Reach out** if you want to take on a specific issue — open a comment claiming it so we don't duplicate work.

## How to help (once code exists)

This section will be filled out at the end of M0, when there's a real codebase to contribute to. Expected workflow:

- Fork → branch (`feat/…`, `fix/…`, `chore/…`) → PR against `main`.
- `cargo fmt` + `cargo clippy` + `cargo test` must pass.
- Frontend changes need `pnpm build` + `pnpm test` to pass.
- New features need tests; bug fixes need regression tests.
- Sign your commits (DCO).
- One logical change per PR.

## Code of Conduct

We follow the [Contributor Covenant 2.1](./CODE_OF_CONDUCT.md). Be excellent to each other.

## License

By contributing, you agree that your contributions will be licensed under [AGPL-3.0](./LICENSE).
