# Photograph

A native Rust (egui) photo browser and editor, built on Patina (`patina-ui`). Read `README.md`
first, then `docs/adr/` for why things are as they are.

This is an open source project that lives on GitHub. It is not one of the Sector F Labs projects,
and the rules in `~/Src/com/sectorflabs/AGENTS.md` do not apply here.

## Remotes

- `origin` is GitHub (`divanvisagie/Photograph`). Development, releases and CI all happen there,
  and the Makefile's `release` target assumes it.
- `urd` is a private copy on urd. Push to it by hand when you want it current; nothing depends on it.

This differs from the Sector F repos, where `origin` is urd and `github` is GitHub's.

## Patina

Photograph depends on a **crates.io release** of `patina-ui` (`patina-ui = "x.y.z"` in
`Cargo.toml`), not Patina's `develop` branch. To take a new one, bump the version, run
`cargo update -p patina-ui`, and read `CHANGELOG.md` in the Patina repo: its **Upgrading** lines
say what to change. When Photograph needs something from Patina, change Patina, release it, then
bump here.

## Commands

- `make test`: the tests. Run it before every commit.
- `make dev`: run with live reload.
- `make screenshot`: save `target/screenshot-*.png`.
- `make release`: bump, tag and push a release, then publish the crate; CI builds the `.deb` and
  `.dmg` (`BUMP=minor|major`, `V=x.y.z`). Run it from `master`, clean and pushed. It makes the
  release commit itself, so don't bump the version first. See ADR-0024.
- `make dmg` builds the macOS package, and needs `rsvg-convert` (`brew install librsvg`).

## Conventions

- **Don't run `cargo fmt` over the repo.** The code isn't rustfmt-formatted, so it rewrites a dozen
  unrelated files. Format only what you write.
- `cargo clippy -- -D warnings` fails on code that was already here; compare against a stash
  before treating a failure as yours.
- A macOS menu bar, window buttons and the `.app` name come from Patina and the bundle's
  `packaging/macos/Info.plist`. A bare `cargo run` binary shows its file name (`photograph`) in the
  menu bar; the packaged app shows `Photograph`.
- To try a change on macOS, rsync the tree to `m3.local` (same `~/Src` layout, excluding `target`
  and `.git`) and build there.
