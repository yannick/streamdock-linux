# Contributing

Thanks for your interest. A few things that help:

- **Build and check:** `just check` runs everything CI would run (rustfmt, clippy with pedantic lints and
  complexity limits, `buf lint`, ESLint, `tsc` and all tests). `just install-hooks` runs `just lint` before
  every commit. The toolchain is pinned in `rust-toolchain.toml`; the web UI needs Node and pnpm.
- **Never test on someone's real dock by accident:** a dock belongs to one daemon (opening it clears its
  keys). For UI work use the simulated dock: `just dev`, or `STREAMDOCKD_FAKE_DEVICE=1 STREAMDOCKD_HARDWARE=0
  just run`. A dev config with real integrations (Home Assistant, Lyrion, HTTP keys) sends real calls.
- **Architecture and dependencies** are fixed in [ADR-001](docs/adr/0001-architecture-and-libraries.md). A new
  dependency updates that ADR in the same change.
- **API changes** start in `proto/` (`buf lint`), then `cd web && pnpm gen`.
- **UI** comes from the design system (`docs/design-system.md`, live at `/design`): blue and green only,
  mobile first.
- **Vendor software:** static analysis only. Never run vendor binaries or firmware updaters.

## Ticket references (`H15-…`)

Comments, ADRs and the changelog mention ids like `H15-36`. They are ticket ids of the project's internal
tracker, where the work was planned before the code was published. They mark *why* something is the way it
is; the explanation that matters is always next to the id, so you don't need access to the tracker. New work
is tracked in this repository's issues.
