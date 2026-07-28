# `cs` vs `ws` — head-to-head re-run

**Measured:** 2026-07-27 — two rounds. Round 1 after the audit fixes, Round 2 after
Stage 4/5/6. **Round 2 is the current state; read it first.** Round 1 is retained
because Round 2 corrects one of its conclusions.
**`ws`:** `audit-fixes` @ `24c3664` (v0.2.0 + 12 unreleased commits) · 400 tests, clippy clean
**`cs`:** upstream **v2026.7.26** (installed binary still `2026.7.24`)

This is the Stage 8 / Changeset 9 re-run. Measurements were re-taken, not copied
from `2026-07-27-cs-vs-ws-comparison.md`. Where a number here disagrees with that
document, this one supersedes it.

---

---

# Round 2 — after Stage 4/5/6

**`ws`:** `audit-fixes` @ `24c3664` · 400 tests · **`cs`:** v2026.7.26, **unchanged**

`cs` did not move this round — first time in three measurements. Its baseline is
genuinely stable here, so every delta below is on the `ws` side.

## R2.1 Interprocess transactions — this axis has reversed

Round 1 scored this to `cs` and named it "the only remaining axis where `cs` is
safer by construction." That was **wrong**, and checking rather than assuming is
what found it.

`cs` has exactly two `noclobber` O_EXCL locks and **both are in `bin/cs-secrets`**.
Nothing in `lib/` or `hooks/` takes an interprocess lock. The `session.lock` files
that appear throughout `lib/15-lock.sh` and `lib/30-worktree.sh` are a **pid-based
mutual-exclusion lock for the whole session** — it reads a pid and tests
`kill -0` — which is the same role as `ws`'s `lock::acquire`, not a
read-modify-write guard.

Meanwhile `cs`'s own machine-local state file is written like this
(`lib/40-state.sh:55-66`):

```sh
_set_local_state() {
    local state="$1" key="$2" value="$3"
    local tmp="$state.tmp"
    {
        if [ -f "$state" ]; then
            awk -v key="$key" 'index($0, key ":") != 1' "$state"
        fi
        printf '%s: %s\n' "$key" "$value"
    } > "$tmp" && mv "$tmp" "$state"
}
```

That reads the existing file to filter out the old line, then renames its result
into place — a read-modify-write behind an atomic rename and no lock. It has two
independent writers in different processes, which `lib/40-state.sh`'s own comment
names: `cs` itself, and `hooks/session-start.sh`'s `local_state_set`. Two of them
interleaving lose one update. **It is the same defect `ws` just fixed in
`contract::update_state`, still open in `cs`.**

So the honest scoring is now split rather than one-sided:

- **Breadth of transactional coverage → `ws`.** Registry, config,
  `workspace.toml`, `state.toml` and the encrypted secret store all hold a lock
  across the whole read-modify-write. `cs` covers its secret store only.
- **Depth of concurrency *testing* → `cs`, clearly.** `cs-secrets` has ten
  deterministic concurrency tests using slow-`openssl` shims and marker barriers.
  `ws` has one lost-update test plus three mechanism tests. `ws` has the better
  coverage and `cs` has the better evidence.

## R2.2 What else moved

| Axis | Round 1 | Round 2 | Why |
|---|---|---|---|
| Interprocess transactions | `cs` | **`ws`** (breadth) / `cs` (test depth) | R2.1 |
| Conversation lineage | `cs`, by more | `cs`, narrower | `ws -conversations` exists now, with `rotated` events and a `from`/`handoff`-carrying `agent-switch`. `cs` still has in-process rotation (`rotate` + `/clear`), which `ws` has no equivalent for. |
| Platform reach | `cs` | `cs`, narrower | `ws` went 1 → 2 release targets (`aarch64-apple-darwin`, `x86_64-unknown-linux-musl`) and now tests on `ubuntu-24.04`. `cs` ships 4 (adds `x86_64-apple-darwin` and `x86_64-pc-windows-msvc`) and its shell runs under WSL and MSYS. Intel Macs and Windows are still `cs`-only. |
| Disclosure path | `cs` | **parity** | `SECURITY.md` added; `cs` is public with an issue tracker. |
| Test mass | `cs`, 3.6× | `cs`, 3.4× | 400 vs 1,374. Narrowing, slowly. |
| Distribution | `cs` | `cs` | Unchanged and still decisive: public + MIT + `curl` vs a private repo needing `gh auth`. |

## R2.3 Numbers

| | `ws` R1 | `ws` R2 | `cs` v2026.7.26 |
|---|---:|---:|---:|
| Production lines | 7,547 | **7,929** | 22,215 |
| Test lines | 8,139 | **8,454** | 20,190 |
| Test-to-production | 1.08 : 1 | 1.07 : 1 | 1.65 : 1 |
| Test cases | 386 | **400** | 1,374 |
| Files | 40 + 17 | 42 + 18 | 27 `lib/` + 10 hooks + 4 bins |
| Release binary | 5,334,496 B | 5,356,880 B | 1,321,740 B (5 executables) |
| Release targets | 1 | **2** | 4 |

Startup, same crude 30-iteration bash harness. **The floor moved between rounds**
(7.59 ms → 4.61 ms for `/usr/bin/true`) because machine load differs, so treat
only the ratio as meaningful and ignore the absolutes:

| | measured | above floor |
|---|---:|---:|
| `ws --version` | 7.79 ms | ≈3.2 ms |
| `cs -version` | 15.35 ms | ≈10.7 ms |
| `ws --help` | 7.12 ms | ≈2.5 ms |
| `cs -help` | 24.72 ms | ≈20.1 ms |

`ws` remains roughly 3–8× cheaper to start. Adding `fs2` and two modules cost
about 22 KB of binary and no measurable startup time.

## R2.4 Verdict, round 2

`ws` now leads on **four** axes rather than two: working Codex support, secret
handling *safety*, breadth of transactional coverage, and startup cost. Round 1's
"only remaining axis where `cs` is safer by construction" no longer exists — and it
turned out `cs` was never ahead there outside its secret store.

What has not budged is the thing that decides adoption. `cs` is public, MIT,
`curl`-installable, on 4 platforms, with 99 releases and 3.4× the tests. `ws` is a
private repository that needs `gh auth` and a grant from its author. Every
engineering axis can be won and that one fact still means `ws` has one user.

**Recommended next, unchanged in substance and now shorter:** make the repository
public and sign the release assets. Then shell completions and the `cargo fmt`
gate. There is no remaining *correctness* item on the critical path — the queue's
schema-validated disposition and Codex's exact session identity are the two
standing trust findings left, and both are bounded and known.

---

## 1. What moved on each side

**`ws`** — 7 commits: Codex secret redaction fixed (matchers moved behind
`Agent::tool_matcher`, `apply_patch` envelope parsing), `-spawn` shell injection
closed, name validation moved to an allowlist at both write chokepoints, M7
manifest clobber fixed, redaction failures no longer discarded, `lock::acquire`
TOCTOU closed, and a truth-in-advertising pass (help, config keys, `-resume`,
README, design doc).

**`cs`** — two releases *during* this work, v2026.7.25 and v2026.7.26, both on
conversation rotation: rotation now happens **in-process** (`rotate` then
`/clear`, no exit and no relaunch), a second rotation supersedes the first, and
seven fixes to the handoff-marker state machine — including rejecting a
`pending-handoff` marker containing a path separator, and fixing a notice that
told a `/compact`-ed conversation it was a clean break.

That second item matters for the comparison: **the conversation-lineage gap
widened while `ws` was closing others.**

---

## 2. Size and tests, same method both sides

| | `ws` | `cs` v2026.7.26 |
|---|---:|---:|
| Authored production | **7,547** lines Rust | **22,215** lines (12,238 shell + 9,977 Rust TUI) |
| Generated/assembled artifact | — | 6,051 (`bin/cs`, excluded from the above) |
| Test lines | **8,139** (5,876 inline + 2,263 `tests/`) | **20,190** |
| Test-to-production ratio | 1.08 : 1 | 1.65 : 1 |
| Test cases | **386** | **1,374** (1,101 shell + 273 Rust) |
| Source files | 40 `src/` + 17 `tests/` | 27 `lib/` fragments + 10 hooks + 4 bins |
| Shipped binary | 5,334,496 bytes (one binary) | 1,321,740 bytes (five executables) |
| Releases | 4 | **99** |
| Commits | 26 | ~980 |

`ws` added 20 tests (366 → 386) and ~300 production lines. The test-count deficit
is **3.6×**, essentially unchanged from the 3.7× in the previous comparison — the
new tests kept pace with the new code rather than closing the gap.

## 3. Startup cost, re-measured

`hyperfine` is not installed here, so timings come from a 30-iteration bash loop.
**That harness has a floor:** `/usr/bin/true` measures **7.59 ms** per iteration,
which is fork/exec overhead, not the program. Floor-subtracted figures, ±1 ms:

| Command | measured | floor-subtracted |
|---|---:|---:|
| `ws --version` | 7.96 ms | **≈0.4 ms** |
| `cs -version` | 15.35 ms | ≈7.8 ms |
| `ws --help` | 6.62 ms | **below the floor** (indistinguishable from process spawn) |
| `cs -help` | 24.09 ms | ≈16.5 ms |

`ws` startup is essentially free; `cs` pays roughly 8 ms to answer `-version` and
16 ms for `-help`. The direction matches the previous comparison's hyperfine
numbers, but **the absolute values in that document are not comparable to these**
— do not mix the two tables. The synthetic 100-workspace listing was not re-run.

## 4. Corrected finding: cs's stale-instruction bug is narrower than reported

The independent audit called this cs's worst flaw. Re-checked at v2026.7.26, and
the earlier description was **imprecise in cs's favour** on one point and
confirmed on the more important one:

- **The shipped template is clean.** `grep` for "Artifact Auto-Tracking",
  "automatically saved to" and "automatically detected" across `lib/*.sh` returns
  **nothing**. New sessions do not get the false claims.
- **There is still no refresh path for existing sessions.** This session's live
  `CLAUDE.local.md` still carries the block (2 matches), still telling its agent
  that Write is auto-redirected to `.cs/artifacts/` and that secrets are
  "automatically detected and stored securely" — both false, and the retired
  hook's own note says the redirect never worked.
- `tests/test_prune_commands.sh:100` still asserts the section is retained, though
  in context that test is checking that pruning the command-tracker section leaves
  *unrelated* sections alone; the artifact block is its example, not its subject.

So the accurate statement is: **cs fixed the template and never migrated existing
sessions.** The damage is real but bounded to sessions created before v2026.7.5,
and it is a missing migration rather than an ongoing lie to new users. That is a
materially better position than the audit implied.

Two other cs findings **persist unchanged** at v2026.7.26: the master password is
still passed on argv at four `openssl -pass "pass:$…"` sites, and **no hook has a
`command -v jq` guard** despite all ten depending on `jq`.

## 5. Axis-by-axis, what actually changed

| Axis | Previous edge | Now | Why |
|---|---|---|---|
| Codex support | `ws` (nominal) | **`ws`, and now real** | Redaction, bash audit and session context verified firing on Codex against CLI 0.145.0. `cs` has zero Codex support by construction. |
| Secret-handling **safety** | `cs` | **`ws`** | `ws`: AES-256-GCM + Argon2, no argv exposure, fail-loud on rewrite/manifest failure, corrupt manifest refused, and it covers Codex. `cs`: unauthenticated AES-256-CBC file backend, password on argv, unknown subcommands exit 0. |
| Secret-handling **breadth** | `cs` | `cs` | More backends, encrypted cross-machine sync, import/export, migration. |
| Command-surface honesty | `cs` | **parity** | `ws` closed three placebo config keys, a no-op flag, an incomplete `--help` and a stale README. |
| Injection / input validation | `cs` | **parity** | `-spawn` no longer goes through `sh -c`; names validated at both chokepoints. |
| Launch-lock correctness | `cs` | **parity** | TOCTOU closed with `O_CREAT\|O_EXCL` on both the create and the stale-reclaim path. |
| Interprocess transactions | `cs` | `cs` | `ws` still has no lock held across read-modify-write for registry, config, meta, state, mail or queue. Atomic rename is not a transaction. `cs-secrets` has real `noclobber` locks with 10 deterministic concurrency tests. |
| Conversation lineage | `cs` | **`cs`, by more** | v2026.7.25 added in-process rotation and a 4-state handoff machine. `ws` still picks the newest handoff by mtime. |
| Test mass | `cs` | `cs` | 3.6× deficit, unchanged. |
| Platform reach | `cs` | `cs` | `ws` is macOS/arm64 only; Linux still not built or tested. |
| Distribution | `cs` | `cs` | `cs` is public, MIT, `curl`-installable, 99 releases. `ws` is a private repo needing `gh auth`. |
| Release authentication | `cs` (upgrade path) | `cs` | Unchanged on both sides. `cs`'s *first-install* path is still unverified — the standing opening for `ws`. |
| Checkpoints, `-live`, usage attribution, completions, doctor depth | `cs` | `cs` | Untouched. |
| Startup speed | `ws` | **`ws`** | Re-measured; `ws` startup is below fork/exec noise. |

## 6. Verdict

**What changed is credibility, not maturity.** Before this branch, `ws`'s single
headline feature did not work, three config keys were placebo, a flag was a no-op,
`--help` hid a third of the surface, and `-spawn` had a reachable command
injection. All of that is now closed and covered by tests. `ws` does what it says.

**`ws` has two axes where it is now genuinely ahead, not just faster:** working
Codex support that `cs` cannot have, and secret-handling *safety* — authenticated
encryption, no argv exposure, fail-loud failure paths, and coverage of both
agents. The second is a reversal from the previous comparison.

**The maturity gap is unchanged and mostly unbuyable at this rate.** 3.6× fewer
tests, one platform, a private repo, 4 releases against 99. `cs` shipped two
releases *during* this session, which is simultaneously why it stays ahead and the
clearest illustration of its bus-factor-1 risk.

**One gap grew.** `cs`'s in-process rotation makes conversation continuity better
on the one axis `ws` exists to own. `ws`'s "newest handoff by mtime" is now
further behind than when the audit was written, and it is the most valuable
remaining feature to build — more valuable than checkpoints.

**Recommended next, in order:** the `transaction()` layer (the last standing trust
finding, and the only remaining item where `cs` is safer by construction);
conversation lineage from timeline `rotated` events; then Linux CI and a public
repo, because nothing else in this document changes until `ws` can be installed by
someone who is not its author.
