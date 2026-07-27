# ws mandatory hardening plan

**Goal:** Close the mandatory correctness, security, session, automation,
diagnostic, release, and documentation gaps found in the 2026-07-26 `cs` vs
`ws` audit, then repeat the audit with the same measurements.

**Baseline:** `phase8-orchestration`, package `0.1.2`, 333 passing tests,
Clippy clean, repository-wide rustfmt failing, prebuilt release limited to
Apple Silicon macOS, and v0.1.2 still a draft.

## Safety rules

1. An atomic rename is not a transaction. Every shared read-modify-write must
   hold an interprocess lock from the first read through the durable rename.
2. A missing file may mean empty state. An unreadable or unparseable file never
   does.
3. Security operations fail visibly. No credential delete, purge, store,
   redaction, rollback, or manifest error is discarded.
4. Session builders are side-effect free. A session id becomes durable only
   after the agent proves that session started.
5. Interactive and unattended session lineages are separate.
6. A queue task is complete only when the CLI exits successfully and the
   agent returns a schema-validated `completed` disposition.
7. Claims in README and the active design document describe shipped behavior,
   not planned behavior.
8. Release assets and the updater bootstrap are authenticated, not merely
   downloaded over TLS.

## Changeset 1: interprocess transactions

**Files:** `Cargo.toml`, `Cargo.lock`, `src/lock.rs`, `src/atomic.rs`,
`src/registry.rs`, `src/config.rs`, `src/meta.rs`, `src/contract.rs`,
`src/context.rs`, `src/readme.rs`, `src/hooksetup.rs`, `src/queue.rs`,
`src/timeline.rs`, `src/mail.rs`, `src/internal.rs`.

- Add a cross-platform advisory file-lock guard and a `transaction(path, f)`
  helper with bounded wait and contextual errors.
- Make workspace launch acquisition race-free by serializing stale-owner
  inspection/reclamation and using `create_new` for the PID record.
- Lock registry, config, metadata, session state, queue/timeline appends, mail
  acknowledgement, context/readme edits, hook configuration, backups, and
  artifact-manifest updates.
- Keep read-only paths lock-free.

**Acceptance:**

- Two simultaneous launch-lock acquisitions produce exactly one owner.
- Concurrent registry/config/tag/session/queue writers preserve every update.
- Tests run with at least four Rust test threads; no project-wide
  `RUST_TEST_THREADS=1` override remains.
- Corrupt and unreadable-file refusal behavior remains intact.

## Changeset 2: transactional secrets and fail-closed redaction

**Files:** `src/secrets.rs`, `src/internal.rs`, `tests/secrets.rs`,
`tests/redact.rs`.

- Serialize file-store and keyring-index mutations.
- Introduce an injectable credential-vault boundary so deletion and rollback
  failures are testable without touching the user's real keychain.
- For keyring set/remove/purge, capture prior values and compensate on a later
  index failure. Report both the primary and rollback error if compensation
  also fails.
- Treat `NoEntry` as idempotent absence; propagate every other vault error.
- File purge propagates every error except `NotFound`.
- Redaction stores all detected values, durably rewrites the file, and records
  the manifest. Any failure exits non-zero with the affected path/name.
- If file rewriting fails, restore previous secret values or remove newly
  created entries; report rollback failures.

**Acceptance:**

- Concurrent file-store sets never lose a secret.
- Injected keyring get/set/delete/index failures never report false success.
- A redaction failure cannot return success while plaintext remains.
- Values never appear in stdout, stderr, manifests, journals, or test logs.

## Changeset 3: exact verified agent sessions

**Files:** `src/agents/mod.rs`, `src/agents/claude.rs`,
`src/agents/codex.rs`, `src/commands.rs`, `src/internal.rs`,
`src/contract.rs`, `tests/launch.rs`, `tests/internal.rs`.

- Export `WS_AGENT` for launches and drains.
- Remove all pre-launch session writes and the Codex `launched` marker.
- SessionStart writes the exact hook `session_id` under the named interactive
  agent only after the agent starts.
- Resume Codex with `codex resume <uuid>`, never `--last`.
- Keep Claude's preselected `--session-id` argument ephemeral until its
  SessionStart confirmation.
- Parse `codex exec --json` `thread.started.thread_id` for the separate
  `codex-drain` lineage; parse Claude's returned `session_id` for
  `claude-drain`.
- Invalid/orphaned stored ids produce a clear error or a fresh session, never
  an arbitrary resume.

**Acceptance:**

- A failed binary launch leaves no durable session state.
- A later unrelated Codex session cannot change what `ws` resumes.
- Interactive launches never resume drain sessions and drains never resume
  interactive sessions.
- Concurrent SessionStart writes preserve other agent state.

## Changeset 4: schema-validated queue results

**Files:** `src/agents/mod.rs`, `src/agents/claude.rs`,
`src/agents/codex.rs`, `src/drain.rs`, `src/queue.rs`, tests.

- Define `completed`, `blocked`, and `failed` result dispositions with a
  required non-secret summary.
- Use Claude `--json-schema` and Codex `--output-schema`.
- Parse Codex JSONL terminal events independently of the final structured
  message. `turn.failed`, `error`, missing `turn.completed`, invalid schema,
  missing output, or non-zero exit all fail.
- Store the disposition and sanitized summary as task evidence and in the
  local drain journal.
- Keep failure circuit breakers and the iteration cap.

**Acceptance:**

- Nonempty prose, refusals, malformed JSON, and false `completed` transport
  states do not mark a task done.
- Only schema-valid `completed` plus a successful terminal event marks done.
- Blocked/failed tasks retain actionable evidence without leaking secrets.

## Changeset 5: identity, mail, and doctor

**Files:** `src/workspace.rs`, `src/commands.rs`, `src/secrets.rs`,
`src/mail.rs`, `src/internal.rs`, doctor tests and workspace tests.

- Export one public workspace-name validator:
  `[A-Za-z0-9][A-Za-z0-9._-]{0,63}`.
- Apply it to create, adopt, registry mutation, explicit command names, and
  secret namespaces.
- Resolve the current secret namespace from `.ws/workspace.toml`; directory
  basename is only an error-reporting fallback, never identity.
- Stop acknowledging mail during SessionStart injection. Add explicit
  acknowledgement after display through the CLI and preserve unread mail when
  launch/injection fails.
- Expand doctor to check config/registry parsing, workspace identity/path
  consistency, contract versions, local ignore rules, lock owners, queue and
  mail readability, secret backend reachability, hook JSON and exact commands,
  shim executability, Claude/Codex status lines, and release/platform support.
- Distinguish hard failures from actionable warnings in output and exit status.

**Acceptance:**

- Invalid names cannot enter TOML, filenames, registry keys, or keyring
  services.
- Adopted aliases use the registered identity for secrets.
- Injected mail remains unread until explicitly acknowledged.
- Each doctor hard-failure branch has an integration test.

## Changeset 6: concurrency and quality gates

**Files:** `.cargo/config.toml`, `.github/workflows/ci.yml`, Rust sources/tests.

- Remove the global single-thread test setting and explicitly serialize only
  tests that mutate process-global environment.
- Add child-process concurrency tests for registry, state, queue, and encrypted
  secrets.
- Run rustfmt once across the repository and add `cargo fmt --all -- --check`
  before Clippy.
- Run tests on macOS and Linux. Keep unsupported platforms explicitly
  unsupported rather than implying coverage.
- Add a dependency/security audit gate with a reviewed advisory policy.

**Acceptance:**

- `cargo test -- --test-threads=4`, fmt, Clippy, release build, and all
  integration tests pass locally.
- CI expresses the same gates and platform claims as the README.

## Changeset 7: authenticated multi-platform releases

**Files:** `.github/workflows/release.yml`, `install.sh`, `src/update.rs`,
release/update tests, README.

- Publish releases non-draft after every build, checksum, and attestation step
  succeeds.
- Build documented macOS and Linux targets; map `uname` values exactly in the
  installer. Unsupported targets fail with an accurate source-build message.
- Include the archive and installer in SHA256SUMS.
- Create GitHub artifact attestations for the installer, checksum manifest,
  and binaries. Since `gh` is already required for this private repository,
  updater verifies attestations with `gh attestation verify` before executing
  the downloaded installer.
- Make update stage the new binary, verify its version/doctor, then atomically
  replace the existing executable.

**Acceptance:**

- A tampered installer, checksum file, or archive is rejected.
- Failed verification leaves the current executable untouched.
- A tag creates a published release or fails; it cannot silently leave the
  updater pointed at an older version.

## Changeset 8: truthful product contract

**Files:** `README.md`, `CHANGELOG.md`,
`docs/specs/2026-07-24-agent-workspaces-design.md`, config/CLI code.

- Mark the design document as historical/aspirational and add an implemented
  capability matrix.
- Remove Clap, Gemini, completions, usage, and other unimplemented claims from
  current architecture text, or label them explicitly planned.
- Remove dead config keys or implement them. No accepted setting may have no
  runtime effect.
- Document exact supported platforms, runtime commands, queue semantics,
  secret failure behavior, lock behavior, and recovery steps.

**Acceptance:**

- Every documented command is present in `ws --help` and parses.
- Every exposed config key has an effect and a behavioral test.
- A grep-based claim audit finds no unqualified shipped claim for planned work.

## Changeset 9: repeat the comparison

**Interim checkpoint:** `docs/2026-07-27-cs-vs-ws-comparison.md` corrects the
post-Phase-8 feature matrix and baseline. It is not the Changeset 9 acceptance
run; repeat every measurement after Changesets 1–8.

- Re-run the exact v2026.7.24 `cs` source/test baseline.
- Recount authored production/test/docs/generated lines with the original
  method.
- Repeat warm version/help/empty-list/100-workspace/statusline benchmarks.
- Re-score completeness, architecture, performance, tests/CI, data integrity,
  cross-agent continuity, distribution, UX/doctor, and documentation honesty.
- Record remaining issues without discounting them because they are known.

**Exit criterion:** `ws` may be called a trustworthy cross-agent beta only if
all P0 findings are closed, the full local gate is green, and updater/release
authentication is implemented. It may be called a full `cs` replacement only
if the repeated feature comparison supports that statement; hardening alone
does not imply feature parity.
