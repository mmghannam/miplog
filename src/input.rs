//! Input sources for log text.
//!
//! Currently supports strings and files (plain or gzipped). Folder walking
//! and zip extraction are on the roadmap — the API is shaped so those can be
//! added without breaking callers.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

/// Read a log file to a string, transparently decompressing `.gz`.
pub fn read_file(path: impl AsRef<Path>) -> io::Result<String> {
    let path = path.as_ref();
    let mut f = File::open(path)?;
    let is_gz = path.extension().and_then(|s| s.to_str()) == Some("gz");
    if is_gz {
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        let mut d = flate2::read::GzDecoder::new(&buf[..]);
        let mut s = String::new();
        d.read_to_string(&mut s)?;
        Ok(s)
    } else {
        let mut s = String::new();
        f.read_to_string(&mut s)?;
        Ok(s)
    }
}

/// A single entry in a concatenated log — one instance's output.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ConcatEntry {
    /// Instance identifier from the run script (e.g. "modified/p_30n20b8.mps.gz").
    pub instance: String,
    /// The full text for this instance, start marker included.
    pub text: String,
}

/// Split Mittelmann-style concatenated logs into per-instance chunks.
///
/// The benchmark driver delimits instances with `@01 <path> ===========`
/// lines. We split on those and keep the marker as the first line of each
/// chunk so parsers see a standalone log.
pub fn split_concatenated(text: &str) -> Vec<ConcatEntry> {
    let mut out = Vec::new();
    let mut cur_name: Option<String> = None;
    let mut cur_text = String::new();
    for line in text.split_inclusive('\n') {
        if let Some(rest) = line.strip_prefix("@01 ") {
            if let Some(name) = cur_name.take() {
                out.push(ConcatEntry {
                    instance: name,
                    text: std::mem::take(&mut cur_text),
                });
            }
            // Marker line: "@01 <path> ==========="; strip trailing " ==...".
            let name = rest
                .trim_end_matches('\n')
                .trim_end_matches(|c: char| c == '=' || c.is_whitespace())
                .trim()
                .to_string();
            cur_name = Some(name);
            cur_text.push_str(line);
        } else if cur_name.is_some() {
            cur_text.push_str(line);
        }
    }
    if let Some(name) = cur_name {
        out.push(ConcatEntry {
            instance: name,
            text: cur_text,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_finds_entries() {
        let text = "preamble ignored\n\
            @01 modified/foo.mps.gz ===========\n\
            foo log line\n\
            @05 7200\n\
            @01 modified/bar.mps.gz ===========\n\
            bar log line\n";
        let entries = split_concatenated(text);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].instance, "modified/foo.mps.gz");
        assert!(entries[0].text.contains("foo log line"));
        assert_eq!(entries[1].instance, "modified/bar.mps.gz");
        assert!(entries[1].text.contains("bar log line"));
    }

    #[test]
    fn split_empty_when_no_markers() {
        assert!(split_concatenated("random text\n").is_empty());
    }
}
