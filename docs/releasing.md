# Releasing, signing, and going public

Two things need a human: publishing a signing key, and flipping repository
visibility. Everything else is already wired. Do them in this order — a public
repository with unsigned releases is a worse position than a private one, because
strangers can now download assets nobody can verify.

---

## 1. Generate and publish the signing key

Done once, on a machine you trust. **Not** in CI.

```sh
brew install minisign          # or: sudo apt-get install minisign
cd ~/.ws-release-key           # somewhere outside the repository
minisign -G -W -p minisign.pub -s minisign.key
```

`-W` creates the key with **no password**, which is what lets CI sign without a
second secret. That is a deliberate trade: the secret key becomes a
single-factor credential, so it must live only in GitHub Actions secrets and in
an offline backup, never in the repository, a password manager note pasted into
chat, or a scratch file.

```sh
cat minisign.pub    # two lines; the second is the key itself
```

Take the **second line** — the base64 blob, not the `untrusted comment:` line — and
set it as the default in `install.sh`:

```sh
# in install.sh
MINISIGN_PUBKEY="${WS_MINISIGN_PUBKEY:-RWQf6...}"
```

Commit that. The public key living in the repository is the point: trust is
established when someone obtains the repository, and from then on a compromised
release host can replace assets but cannot forge a signature matching it.

Then store the secret key:

```sh
gh secret set MINISIGN_SECRET_KEY --repo djock/agent-workspaces < minisign.key
```

Back `minisign.key` up somewhere offline. If it is lost, every future release
needs a new key and every existing installer needs updating — recoverable, but
only by hand.

**Never** commit `minisign.key`. Add a guard so a stray copy cannot be staged:

```sh
printf 'minisign.key\n*.key\n' >> .gitignore
```

### Verify it end to end before trusting it

Tag a throwaway release and check that the gate actually bites:

```sh
git tag v0.2.1-rc1 && git push origin v0.2.1-rc1
# wait for the workflow, then:
gh release download v0.2.1-rc1 --pattern 'SHA256SUMS*' --dir /tmp/verify
minisign -V -P "$(sed -n 2p ~/.ws-release-key/minisign.pub)" \
         -m /tmp/verify/SHA256SUMS -x /tmp/verify/SHA256SUMS.minisig
```

Then prove it fails closed, which is the half people skip:

```sh
printf 'tampered\n' >> /tmp/verify/SHA256SUMS
minisign -V -P "$(sed -n 2p ~/.ws-release-key/minisign.pub)" \
         -m /tmp/verify/SHA256SUMS -x /tmp/verify/SHA256SUMS.minisig
# must FAIL. If it passes, the signature is not covering what you think it is.
```

Delete the RC release and tag afterwards.

---

## 2. Make the repository public

Only after step 1 verifies. Before flipping the switch, check the history — the
repository has been private for its whole life, so nothing in it has ever been
reviewed with public eyes.

```sh
# Anything that looks like a credential, anywhere in history:
git log --all -p | grep -nEi '(api[_-]?key|secret|token|password|BEGIN [A-Z ]*PRIVATE KEY)' | head -40

# Files that should never have been committed:
git log --all --name-only --format= | sort -u | grep -Ei '\.(key|pem|p12|env)$|minisign'
```

A hit in history is not fixed by deleting the file in a new commit — the blob is
still reachable. Rotate the credential; rewriting history is a last resort.

Also worth a check before it is public: on every launch `ws` splices a managed
block into the workspace root's context file — `CLAUDE.md` for Claude,
`AGENTS.md` for Codex (`agents::Agent::context_file`, written at
`commands.rs`'s `ensure_context`). This repository is developed from inside a
`ws` workspace whose root *is* the repository, so those files can appear at the
repository root, untracked and unignored, and be committed by accident along
with whatever the managed block contains.

Neither file exists in the tree today. Confirm that still holds, since the
window is one `git add -A` wide:

```sh
git ls-files | grep -E '^(CLAUDE|AGENTS)\.md$'   # must print nothing
ls CLAUDE.md AGENTS.md 2>/dev/null               # and so must this
```

Then:

```sh
gh repo edit djock/agent-workspaces --visibility public --accept-visibility-change-consequences
```

Consequences worth knowing before you type it: stars and forks reset, the repo
becomes indexable and clonable by anyone, and **all existing releases and their
assets become public** — which is why step 1 comes first.

### After going public

`install.sh` currently requires `gh` and an authenticated account, because that
was the only way to read a private repository. Once public it can drop that:
`gh release download` still works unauthenticated, so the immediate change is
just removing the `gh auth status` precondition and the "private repository" line
from the README's requirements.

Do **not** replace it with `curl … | sh`. That pattern is exactly what this
project's own audit criticised in `cs`: it executes the payload before anything
verifies it. Download, verify, then run:

```sh
gh release download v0.2.0 --repo djock/agent-workspaces --pattern install.sh
# inspect it, then:
sh install.sh
```

---

## 3. Cutting a normal release

```sh
# 1. Version and changelog must agree — the workflow asserts tag == Cargo version.
$EDITOR Cargo.toml CHANGELOG.md
cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings

# 2. If src/redact_rule.rs changed, measure it (see below) before going further.

# 3. Push the release commit and WAIT for CI on it to go green.
git push origin main
gh run watch "$(gh run list --branch main --limit 1 --json databaseId --jq '.[0].databaseId')"

# 4. Only then tag. A tag is one-way: it names a commit forever, and deleting a
#    pushed one leaves anyone who already fetched it on a version that no longer
#    exists.
git tag v0.3.0 && git push origin v0.3.0

# 5. The workflow builds both targets, assembles SHA256SUMS, signs it, attests
#    provenance, and creates a DRAFT release. Review the assets, then publish:
gh release view v0.3.0
gh release edit v0.3.0 --draft=false
```

**Wait for CI before tagging, not after.** `cargo test` locally is not the same
check: CI builds the cross-compiled target and runs on a clean checkout, and it
is where a `--locked` failure after a version bump shows up. `cs` tagged two
releases on commits whose CI then went red, and both had to be re-cut — the tag
could not be moved, only orphaned.

### The redaction rule ships a claim about a population

`src/redact_rule.rs` decides what counts as a credential in a file the agent
wrote. A diff-scoped review sees the matcher once and judges whether it *looks*
right; that is how `cs`'s corpus redactor survived twenty-nine releases before
anyone ran it over the transcripts it was protecting and found it had fired
twice, both false positives, catching nothing.

So a change there is not done until it has been measured against a real tree:

```sh
WS_MEASURE_ROOT=~/Projects cargo test --test redact_population -- --ignored --nocapture
```

Read all three lists. Firings must be credentials; the "already redacted" list
must not be shrinking; the miss list must be configuration and documentation,
nothing else. The harness prints names, value *shapes* and paths — never values.

Releases are created as drafts deliberately: an unsigned or half-uploaded release
that is already public cannot be un-published, only deleted.

## What a user can check

```sh
# Signature over the checksums:
minisign -V -P <pubkey> -m SHA256SUMS -x SHA256SUMS.minisig

# Independent, keyless provenance — that this artifact was built by this
# workflow from this repository, verified against GitHub's transparency log:
gh attestation verify ws-v0.3.0-aarch64-apple-darwin.tar.gz --repo djock/agent-workspaces
```

The two are independent on purpose. minisign proves the release key signed it and
works with no network and no GitHub account. Attestation proves which workflow and
commit built it, and needs no key management. A compromise has to defeat both.
