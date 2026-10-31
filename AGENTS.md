# AGENTS.md

Guidance for AI agents working in this repository.

## Start here

Use relevant sections of [`docs/project-structure.md`](docs/project-structure.md)
for orientation and code navigation. Treat it as a guide, not a substitute for
the source: inspect code, tests, and configuration as needed, and use search
when it helps answer the task. If the current change alters documented project
structure or behavior, update the matching section as described below. Flag
unrelated documentation drift without expanding the task.

## Keep the structure doc current

`docs/project-structure.md` is a living document. Update it **in the same
change** whenever a task alters the shape of the project, including:

- Adding, removing, renaming, or moving a crate, module, or top-level source file.
- Changing a crate's responsibility, dependencies, features, or entry point.
- Adding/removing a Slint component or a page-level property/callback pattern.
- Adding/removing persisted data (KV keys, files, sync domains) or changing the
  storage backend.
- Changing the sync protocol (ALPNs, wire schema, peer/membership model).
- Changing platform behavior (desktop vs Android), build/packaging, or features.
- Adding a new "where to look for X" entry that a future agent would need.

Keep edits surgical: update the relevant table/section and the quick-reference
index. Do not rewrite the whole file, and do not add speculative sections for
things that do not exist. If you are unsure whether a change belongs in the doc,
add it only if it helps someone navigate the code without reading it.

## Build & verify

Run the narrowest check that covers your change, then the broader ones:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --locked -- -D warnings
cargo check --workspace --locked
cargo test --locked -p reader-sources
cargo test --locked -p reader-domain -p reader-engine -p reader-downloads -p reader-catalogue
cargo test --locked --test reading_journey
cargo test --locked --test ui_navigation
cargo test --workspace --locked
```

- `make check` also runs Node addon fixtures and Python packaging regressions.
- Android uses `--no-default-features --features android --lib`; keep desktop
  dialogs/dependencies gated from Android.
- Build Android APKs for device delivery in release mode with
  `nix develop .#android --command make android-build-release`, unless the user
  explicitly requests a debug build.
- CI follows Nova's workflow: formatting, Clippy, workspace checks, packaging
  regressions, Windows GNU and Android ARM64 checks run on PRs and main pushes.
  Tag pushes build/package/publish directly; check jobs are skipped on tags and
  build jobs do not depend on them. Preserve this user-requested split when
  adapting `.github/workflows/build-release.yml`.
- Run the narrowest relevant behavior tests before the workspace checks. Headless
  Slint tests belong in `tests/`; leaf-crate tests may live next to their code.

## Conventions & gotchas

- Comments are welcome. Explain why when the rationale is not obvious.
- Keep heavy subsystems in `crates/*` so app orchestration edits do not trigger
  unnecessary recompilation. See `docs/project-structure.md` for ownership.
- Slint access stays on its event loop. Workers fetch/decode, then use
  `slint::invoke_from_event_loop` to publish current-generation results.
- Keep UI models/properties/callbacks in `crates/ui/app.slint` aligned with
  `src/app.rs` wiring. Mark user-facing Slint strings with `@tr`; Rust UI strings
  go through `crates/ui/src/backend_text.rs`.
- Reader identities are source-local strings. Fractional chapter numbers and
  expiring URLs must never replace stable series/chapter/page IDs.
- `nova-storage` currently supports one application directory per process. Use
  subprocesses for restart/directory-isolation tests.
- Application data is independent of Nova: binary `nova-reader`, Android package
  `dev.misob.nova.reader`, native library `nova_reader`, and
  `NOVA_READER_DATA_DIR`. The database may retain the internal filename
  `nova.redb` within Reader's separate directory.
- Reader has no Nova sync wire protocol, media relay or video-library dependency.
  Do not carry those identities or packaging assumptions into Reader.
- Reader format-2 JavaScript packages use `nova-reader1`. Keiyoushi APK/JAR
  catalogue entries are discovery metadata and cannot execute here. Generic
  extraction to kit must retain Nova's separate v1 compatibility gate.

## Git

- **Commit messages:** Use Conventional Commit subjects in the form `type(scope): imperative summary`. Keep the summary concise and lowercase after the colon (preserve proper names), and omit the scope when it does not help identify the change.
  - Types: `feat`, `fix`, `refactor`, `perf`, `ui`, `docs`, `test`, `build`, `ci`, `chore`.
  - Useful scopes include `reader`, `library`, `addons`, `search`, `settings`, `android`, `desktop`, and `release`.
  - Examples: `feat(reader): add vertical paging`; `fix(addons): retain previous package on failed update`; `ci(release): publish Linux .deb and AppImage builds`.
  - Mark breaking changes with `!` after the type or scope, and explain the impact in the body: `feat(addons)!: change reader source contract`.
- Do **not** commit, amend, push, or open PRs unless the user explicitly asks.
- Check `git status` before editing or staging, and preserve unrelated existing
  work.
- Stage only intended files. Do not add credentials or private production
  signing keys.
- `android/keystore/debug.keystore` is the intentionally tracked public debug
  key used by Cargo's Android signing profiles. Preserve it for local/debug
  builds and explicitly authorized personal sideload releases that must update
  existing installs. Store releases require a private signing-key override.
  Changing an existing personal install's signing identity requires an explicit
  migration plan; do not silently replace its key during a version bump.
