use anyhow::Result;
use std::path::PathBuf;

pub const PROMPTS: &[(&str, &str)] = &[
    ("summary.md", include_str!("assets/prompts/summary.md")),
    ("wrap.md", include_str!("assets/prompts/wrap.md")),
    ("sweep.md", include_str!("assets/prompts/sweep.md")),
    ("rotate.md", include_str!("assets/prompts/rotate.md")),
    // `/ws:task` — capture a task mid-turn without switching to it.
    ("task.md", include_str!("assets/prompts/task.md")),
];

pub fn commands_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("commands")
        .join("ws")
}

/// Install the shared ws prompt bodies into `dir`, naming each file via `filename_of(base)`
/// where `base` is the prompt name with any `.md` suffix stripped (e.g. "summary").
pub fn install_for(dir: &std::path::Path, filename_of: impl Fn(&str) -> String) -> Result<usize> {
    std::fs::create_dir_all(dir)?;
    for (name, body) in PROMPTS {
        let base = name.strip_suffix(".md").unwrap_or(name);
        std::fs::write(dir.join(filename_of(base)), body)?;
    }
    Ok(PROMPTS.len())
}

/// Remove only the prompt files whose names are owned by ws.
pub fn uninstall_for(dir: &std::path::Path, filename_of: impl Fn(&str) -> String) -> Result<usize> {
    let mut removed = 0;
    for (name, _) in PROMPTS {
        let base = name.strip_suffix(".md").unwrap_or(name);
        let path = dir.join(filename_of(base));
        match std::fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    if dir.is_dir() && std::fs::read_dir(dir)?.next().is_none() {
        std::fs::remove_dir(dir)?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn install_writes_all_namespaced_prompts() {
        let d = TempDir::new().unwrap();
        std::env::set_var("HOME", d.path());
        let n = install_for(&commands_dir(), |base| format!("{base}.md")).unwrap();
        assert_eq!(n, PROMPTS.len());
        for (name, _) in PROMPTS {
            let p = commands_dir().join(name);
            assert!(p.is_file(), "missing {name}");
        }
        // namespaced under commands/ws (so /ws:summary, never clobbering cs's /summary)
        assert!(commands_dir().ends_with("commands/ws"));
        assert!(std::fs::read_to_string(commands_dir().join("rotate.md"))
            .unwrap()
            .contains("handoff"));
    }

    #[test]
    fn uninstall_removes_owned_prompts_and_preserves_foreign_files() {
        let d = TempDir::new().unwrap();
        let dir = d.path().join("prompts");
        install_for(&dir, |base| format!("ws-{base}.md")).unwrap();
        std::fs::write(dir.join("mine.md"), "keep").unwrap();

        let removed = uninstall_for(&dir, |base| format!("ws-{base}.md")).unwrap();

        assert_eq!(removed, PROMPTS.len());
        assert_eq!(std::fs::read_to_string(dir.join("mine.md")).unwrap(), "keep");
    }
}
