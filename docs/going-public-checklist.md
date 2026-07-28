# Going public — operator checklist

**Written:** 2026-07-27 · **Branch:** `audit-fixes` @ 14 commits · 400 tests, clippy clean

Tickable steps with the exact commands. This is the *doing* document; the
*reasoning* is in [`releasing.md`](releasing.md) and the gap analysis is in
[`2026-07-27-cs-vs-ws-independent-audit.md`](2026-07-27-cs-vs-ws-independent-audit.md).

Everything below is a human action. The code side is already done — see
[§7](#7-what-is-already-done) for exactly what was built and what it does.

---

## Order matters

**Sign releases before flipping visibility.**

Going public makes **all existing release assets public too**. A public repository
with unsigned releases is a worse position than a private one: strangers can now
download artifacts that nobody can verify. Do §1–§4 before §5.

---

## 1. Generate the signing key

On a machine you trust. **Not** in CI.

- [ ] Install minisign and create the keypair outside the repository:

```sh
brew install minisign                 # or: sudo apt-get install minisign
mkdir -p ~/.ws-release-key && cd ~/.ws-release-key
minisign -G -W -p minisign.pub -s minisign.key
```

`-W` creates the key with **no password**, which is what lets CI sign without a
second secret. The trade: the secret key is single-factor, so it may live only in
GitHub Actions secrets and an offline backup.

- [ ] Back up `minisign.key` offline. Losing it means a new key and a new
      `install.sh` for every future release.

## 2. Publish the public half, store the secret half

- [ ] Copy the **second line** of `minisign.pub` — the base64 blob, *not* the
      `untrusted comment:` line. This is the easy mistake.

```sh
sed -n 2p ~/.ws-release-key/minisign.pub
```

- [ ] Paste it as the default in `install.sh`:

```sh
MINISIGN_PUBKEY="${WS_MINISIGN_PUBKEY:-RWQf6...}"
```

- [ ] Commit that. The public key belongs in the repository: trust is established
      when someone obtains the repo, and from then on a compromised release host
      can swap assets but cannot forge a matching signature.

- [ ] Store the secret key as an Actions secret:

```sh
gh secret set MINISIGN_SECRET_KEY --repo djock/agent-workspaces < ~/.ws-release-key/minisign.key
```

`*.key` and `*.pem` are already in `.gitignore`, verified to block a staged
`minisign.key`.

## 3. Prove the gate bites

Do not skip the negative case — a signature check that never fails is
indistinguishable from no check.

- [ ] Cut a throwaway RC and verify the good path:

```sh
git tag v0.2.1-rc1 && git push origin v0.2.1-rc1
# wait for the workflow
gh release download v0.2.1-rc1 --pattern 'SHA256SUMS*' --dir /tmp/v
PUB=$(sed -n 2p ~/.ws-release-key/minisign.pub)
minisign -V -P "$PUB" -m /tmp/v/SHA256SUMS -x /tmp/v/SHA256SUMS.minisig   # must PASS
```

- [ ] Verify it fails closed:

```sh
printf 'tampered\n' >> /tmp/v/SHA256SUMS
minisign -V -P "$PUB" -m /tmp/v/SHA256SUMS -x /tmp/v/SHA256SUMS.minisig   # must FAIL
```

- [ ] Delete the RC release and tag.

> Already done locally against real `minisign`, all five cases: keygen, sign,
> verify, tampered-`SHA256SUMS` rejected, wrong-key rejected. So the plumbing is
> tested; this step confirms it against *your* key in *your* CI.

## 4. Scan history before flipping

The repository has been private its whole life, so nothing in it has ever been
read with public eyes.

- [ ] Credential scan across all history:

```sh
git log --all -p | grep -nEi '(api[_-]?key|secret|token|password|BEGIN [A-Z ]*PRIVATE KEY)' | head -40
git log --all --name-only --format= | sort -u | grep -Ei '\.(key|pem|p12|env)$|minisign'
```

- [ ] Skim `AGENTS.md`. `ws` splices a managed block into it on every Codex
      launch and it is committed, so whatever is in that block goes public too.

A hit in history is **not** fixed by deleting the file in a later commit — the blob
stays reachable. Rotate the credential; rewriting history is a last resort.

## 5. Flip visibility

```sh
gh repo edit djock/agent-workspaces --visibility public --accept-visibility-change-consequences
```

Consequences: stars and forks reset, the repo becomes indexable and clonable by
anyone, and every existing release asset becomes public — which is why §1–§4 come
first.

## 6. Simplify the installer

`install.sh` requires `gh` and an authenticated account only because the repository
was private. `gh release download` works unauthenticated against a public repo.

- [ ] Remove the `gh auth status` precondition from `install.sh`.
- [ ] Remove the "private repository" line from the README requirements.

**Do not** replace it with `curl … | sh`. That executes the payload before anything
verifies it — the exact pattern this project's own audit criticised in `cs`. The
supported flow stays download → verify → run:

```sh
gh release download v0.3.0 --repo djock/agent-workspaces --pattern install.sh
# inspect, then:
sh install.sh
```

---

## 7. What is already done

No action needed on these; listed so the checklist above is not mistaken for the
whole job.

| Built | Where | Behaviour |
|---|---|---|
| minisign signing of `SHA256SUMS` | `.github/workflows/release.yml` | One signature covers every asset, so the installer needs exactly one verification step. Skipped with a CI **warning** when no key is configured, so a fork still releases. |
| Keyless build provenance | same | `actions/attest-build-provenance` with `id-token: write`. Independent of minisign on purpose: minisign works offline with no GitHub account; attestation proves which workflow and commit built the artifact with no key to leak. A compromise must defeat both. |
| Signature verification, **fail-closed** | `install.sh` | Signature checked *before* the checksum. Missing signature, failed verification, or absent `minisign` all abort. `--allow-unsigned` must be typed. Deliberately stricter than `cs`, whose installer silently skips verification when the verifier is absent while still reading as "verified". |
| Portable checksum verification | `install.sh` | `sha256sum`, else `shasum`, else refuse. |
| Key leak guard | `.gitignore` | `*.key`, `*.pem`; confirmed to block a staged `minisign.key`. |
| Runbook | `docs/releasing.md` | Key generation, the GitHub secret, ordering, history scan, normal release cadence, and what a *user* can verify. |

### One bug found and fixed in the process

Checksum verification hardcoded `shasum`, which is macOS-only. That was introduced
by the Linux-support commit earlier on this branch — so the Linux install path that
commit enabled would have failed at the verification step. Now `sha256sum` or
`shasum`, and a refusal rather than installing an unverified binary if neither
exists.

## What a user can check after all this

```sh
# Signature over the checksums — offline, no GitHub account needed:
minisign -V -P <pubkey> -m SHA256SUMS -x SHA256SUMS.minisig

# Independent provenance — that this artifact was built by this workflow from this
# repository, verified against GitHub's transparency log:
gh attestation verify ws-v0.3.0-aarch64-apple-darwin.tar.gz --repo djock/agent-workspaces
```

## Still open after this

Not blockers for going public, but the honest remainder:

- Shell completions (`ws completions bash|zsh|fish`) — promised by the old design
  doc, present in `cs`, roughly a day.
- The `cargo fmt --check` gate — repo-wide formatting still fails; the gate belongs
  in the same commit that fixes it. Reason recorded in `ci.yml`.
- Two standing trust findings: the queue's schema-validated agent disposition, and
  exact Codex session identity (blocked on `resume --last` having no addressable
  id).
- `x86_64-apple-darwin` (Intel Macs) and Windows remain `cs`-only; `ws` ships two
  targets against `cs`'s four.
