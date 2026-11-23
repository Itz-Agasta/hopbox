//! Background jobs. The remote `<workspace_root>/jobs/<id>/` dir is the source of truth; the
//! local index only remembers which host each job id was started on.

use std::io::{self, Write};
use std::path::Path;

/// Append `<id>\t<host>` to the index at `path`, creating it and its parent.
pub fn record(path: &Path, id: &str, host: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(f, "{id}\t{host}")
}

/// The host a job id was recorded with, if any (the latest entry wins).
pub fn lookup(path: &Path, id: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .rev()
        .filter_map(|l| l.split_once('\t'))
        .find(|(i, _)| *i == id)
        .map(|(_, h)| h.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_record_lookup() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("state/hopbox/jobs.tsv");
        assert_eq!(lookup(&p, "0a1b2c3d"), None);
        record(&p, "0a1b2c3d", "hermes").unwrap();
        record(&p, "11111111", "box").unwrap();
        assert_eq!(lookup(&p, "0a1b2c3d").as_deref(), Some("hermes"));
        assert_eq!(lookup(&p, "11111111").as_deref(), Some("box"));
        assert_eq!(lookup(&p, "22222222"), None);
    }
}
