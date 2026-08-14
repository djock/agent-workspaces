// The `NAME=VALUE` redaction rule: what counts as a credential in a file the
// agent just wrote.
//
// Included verbatim by `src/internal.rs` (which runs it) and by
// `tests/redact_population.rs` (which measures it against a real tree). Not a
// module: an integration test cannot reach a binary's private modules, and a
// second copy for the test to measure would be a rule nobody ships.
//
// A change here is a change to a claim about a population, so it comes with a
// measurement — see `docs/releasing.md`.

/// A `NAME=VALUE` line (VALUE may be quoted). Returns (name, unquoted value) or None.
pub(crate) fn parse_assignment(line: &str) -> Option<(&str, &str)> {
    let t = line.trim();
    if t.starts_with('#') {
        return None;
    }
    let (name, rest) = t.split_once('=')?;
    let name = name.trim();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return None;
    }
    let value = rest.trim().trim_matches('"').trim_matches('\'');
    Some((name, value))
}

/// Substrings that make a NAME credential-shaped. `ACCESS_KEY` is what catches
/// `AWS_ACCESS_KEY_ID` (which `_KEY`-suffix matching missed entirely, because
/// the name does not end there); `DSN` and `WEBHOOK` are here because both
/// routinely carry an embedded credential in their value.
const SECRET_NAME_PARTS: &[&str] = &[
    "SECRET",
    "TOKEN",
    "PASSWORD",
    "PASSWD",
    "PASSPHRASE",
    "APIKEY",
    "API_KEY",
    "ACCESS_KEY",
    "CREDENTIAL",
    "WEBHOOK",
    "DSN",
    "BEARER",
];

/// Value prefixes that identify a credential on their own, regardless of what
/// the name says. Each is a published, issuer-assigned marker: OpenAI `sk-`,
/// GitHub `ghp_`/`gho_`/`github_pat_`, GitLab `glpat-`, Slack `xox*-`, AWS
/// `AKIA`/`ASIA`, Google `AIza`, npm `npm_`, PyPI `pypi-`.
const SECRET_VALUE_PREFIXES: &[&str] = &[
    "sk-",
    "ghp_",
    "gho_",
    "github_pat_",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "xoxs-",
    "AKIA",
    "ASIA",
    "AIza",
    "npm_",
    "pypi-",
];

/// Values that are configuration, never credentials, whatever the name is.
const NON_SECRET_VALUES: &[&str] = &["true", "false", "yes", "no", "on", "off", "none", "null"];

/// Shortest value the generic (prefix-free) branch will treat as a credential.
/// Set to exclude the port numbers, booleans, sizes and enum words that make up
/// almost every false positive, while still catching a short API key.
const MIN_SECRET_VALUE_LEN: usize = 12;

/// The two-signal test: a credential-shaped NAME **and** a credential-shaped
/// VALUE, both required.
///
/// Name alone — what this replaces — redacted `PASSWORD_MIN_LENGTH=8`,
/// `TOKENIZER=gpt2`, `TOKEN_BUDGET=4096` and `SECRET_SCAN_ENABLED=true`,
/// replacing working configuration with a placeholder that (until
/// `ws -secrets restore` existed) nothing resolved. Value alone would redact
/// every long unquoted string in every file the agent writes. Requiring both
/// keeps `AWS_ACCESS_KEY_ID=AKIA…` and `GITHUB_PAT=github_pat_…`.
///
/// Known gaps, deliberately out of scope here: JSON/YAML values, credentials
/// embedded in URLs (`DATABASE_URL=postgres://user:pw@host`), and files written
/// by a Bash heredoc rather than a write tool. Those are documented rather than
/// half-caught — see task 6.
pub(crate) fn is_secret_assignment(name: &str, value: &str) -> bool {
    // An already-redacted line is not a candidate: re-storing the placeholder
    // as if it were the value is how a redacted file loses its secret for good.
    !value.starts_with(PLACEHOLDER_OPEN) && name_looks_secret(name) && value_looks_secret(value)
}

pub(crate) fn name_looks_secret(name: &str) -> bool {
    let u = name.to_ascii_uppercase();
    if SECRET_NAME_PARTS.iter().any(|p| u.contains(p)) {
        return true;
    }
    if u.ends_with("_KEY") || u == "KEY" {
        return true;
    }
    // `PAT` is three letters that live inside PATH, PATTERN, XPATH and PATCH,
    // so it matches only as a whole `_`-separated segment: `GITHUB_PAT` yes,
    // `PATTERN_FILE` and `XPATH_QUERY` no.
    u.split('_').any(|segment| segment == "PAT")
}

/// Values that are instructions to the reader, not credentials.
///
/// The only false-positive class the population measurement turned up:
/// `BOT_TOKEN=your_discord_bot_token` and `TMDB_API_KEY=your_tmdb_api_key_here`
/// in checked-in `.env.example` files, both credential-named, both long enough,
/// neither a secret. Redacting one moves documentation into the secret store and
/// leaves an example file that no longer shows the reader what to put there.
fn value_is_a_placeholder(value: &str) -> bool {
    let v = value.to_ascii_lowercase();
    if v.starts_with("your_") || v.starts_with("your-") || v.starts_with("your.") {
        return true;
    }
    // `<paste-key-here>` and `${SOME_VAR}` are references, not values.
    if (v.starts_with('<') && v.ends_with('>')) || v.starts_with("${") {
        return true;
    }
    ["changeme", "change_me", "change-me", "replace_me", "replace-me", "xxxxxxxxxxxx", "todo"]
        .iter()
        .any(|p| v.starts_with(p))
}

fn value_looks_secret(value: &str) -> bool {
    if value_is_a_placeholder(value) {
        return false;
    }
    if SECRET_VALUE_PREFIXES.iter().any(|p| value.starts_with(p)) {
        return true;
    }
    value.chars().count() >= MIN_SECRET_VALUE_LEN
        && !value.chars().any(char::is_whitespace)
        && !value.chars().all(|c| c.is_ascii_digit())
        && !NON_SECRET_VALUES.iter().any(|w| w.eq_ignore_ascii_case(value))
}

/// The placeholder redaction leaves behind, and the only shape
/// `ws -secrets restore` resolves. Writer and reader live next to each other
/// because a disagreement about this string is a file nothing can repair: the
/// hook has already moved the value into the store by the time it writes one.
pub const PLACEHOLDER_OPEN: &str = "{{ws:secret:";
pub const PLACEHOLDER_CLOSE: &str = "}}";
