use anyhow::Result;

/// Implemented in Task 4. Kept as a distinct module so the queue store and the
/// unattended runner can be reviewed separately.
pub fn run(_name: Option<String>, _reset: bool) -> Result<()> {
    anyhow::bail!("ws -queue drain is not implemented yet")
}
