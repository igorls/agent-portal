use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;

/// Bounded look at a JSONL file: parsed lines from the first and last
/// `window` bytes, never the middle. This is what keeps enumerating a store
/// with thousands of multi-MB transcripts cheap.
pub struct JsonlPeek {
    pub head: Vec<Value>,
    pub tail: Vec<Value>,
    pub size: u64,
    /// Exact non-empty line count (small files read whole).
    pub exact_line_count: Option<u32>,
    /// Extrapolated from newline density in the head window (large files).
    pub estimated_line_count: Option<u32>,
}

pub const DEFAULT_WINDOW: u64 = 48 * 1024;

pub fn peek(path: &Path, window: u64) -> std::io::Result<JsonlPeek> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();

    if size == 0 {
        return Ok(JsonlPeek {
            head: Vec::new(),
            tail: Vec::new(),
            size,
            exact_line_count: Some(0),
            estimated_line_count: None,
        });
    }

    if size <= window * 2 {
        let mut buf = Vec::with_capacity(size as usize);
        file.read_to_end(&mut buf)?;
        let text = String::from_utf8_lossy(&buf);
        let raw_lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        let parsed: Vec<Value> = raw_lines
            .iter()
            .filter_map(|l| serde_json::from_str(l.trim()).ok())
            .collect();
        return Ok(JsonlPeek {
            head: parsed.clone(),
            tail: parsed,
            size,
            exact_line_count: Some(raw_lines.len() as u32),
            estimated_line_count: None,
        });
    }

    // Head window, cut at the last complete line.
    let mut head_buf = vec![0u8; window as usize];
    file.read_exact(&mut head_buf)?;
    let head_text = String::from_utf8_lossy(&head_buf);
    let head_upto = head_text.rfind('\n').unwrap_or(0);
    let head_slice = &head_text[..head_upto];
    let head: Vec<Value> = head_slice
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l.trim()).ok())
        .collect();
    let head_lines = head_slice.lines().filter(|l| !l.trim().is_empty()).count();

    // Tail window, skip the first (probably partial) line.
    file.seek(SeekFrom::End(-(window as i64)))?;
    let mut tail_buf = vec![0u8; window as usize];
    file.read_exact(&mut tail_buf)?;
    let tail_text = String::from_utf8_lossy(&tail_buf);
    let tail_from = tail_text.find('\n').map(|i| i + 1).unwrap_or(0);
    let tail: Vec<Value> = tail_text[tail_from..]
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l.trim()).ok())
        .collect();

    let estimated = if head_upto > 0 {
        Some(((head_lines as f64) * (size as f64) / (head_upto as f64)).round() as u32)
    } else {
        None
    };

    Ok(JsonlPeek {
        head,
        tail,
        size,
        exact_line_count: None,
        estimated_line_count: estimated,
    })
}

/// Read parsed JSON lines from the start of a file, stopping after
/// `max_lines`, after `max_bytes` consumed, or as soon as `stop` matches a
/// parsed line (that line is included). For stores whose opening records are
/// oversized (Codex embeds 100KB+ of instructions in its first lines), this
/// guarantees the early metadata records regardless of their byte size.
pub fn head_lines(
    path: &Path,
    max_lines: usize,
    max_bytes: u64,
    stop: impl Fn(&Value) -> bool,
) -> std::io::Result<Vec<Value>> {
    use std::io::BufRead;

    let file = File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    let mut out = Vec::new();
    let mut consumed: u64 = 0;
    let mut line = String::new();

    for _ in 0..max_lines {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break;
        }
        consumed += n as u64;
        if let Ok(value) = serde_json::from_str::<Value>(line.trim()) {
            let hit = stop(&value);
            out.push(value);
            if hit {
                break;
            }
        }
        if consumed >= max_bytes {
            break;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_jsonl(lines: usize, pad: usize) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("portal-jsonl-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("peek-{lines}-{pad}.jsonl"));
        let mut f = File::create(&path).unwrap();
        for i in 0..lines {
            writeln!(f, r#"{{"i":{i},"pad":"{}"}}"#, "x".repeat(pad)).unwrap();
        }
        path
    }

    #[test]
    fn small_file_is_exact() {
        let path = temp_jsonl(10, 10);
        let p = peek(&path, DEFAULT_WINDOW).unwrap();
        assert_eq!(p.exact_line_count, Some(10));
        assert_eq!(p.head.len(), 10);
        assert_eq!(p.head[0]["i"], 0);
        assert_eq!(p.tail[9]["i"], 9);
    }

    #[test]
    fn large_file_head_tail_and_estimate() {
        let path = temp_jsonl(5000, 100);
        let p = peek(&path, 8 * 1024).unwrap();
        assert!(p.exact_line_count.is_none());
        assert!(!p.head.is_empty());
        assert!(!p.tail.is_empty());
        assert_eq!(p.head[0]["i"], 0);
        assert_eq!(p.tail.last().unwrap()["i"], 4999);
        let est = p.estimated_line_count.unwrap();
        assert!((4000..=6000).contains(&est), "estimate {est} out of range");
    }

    #[test]
    fn head_lines_stops_on_predicate_regardless_of_line_size() {
        let dir = std::env::temp_dir().join("portal-jsonl-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fat-head.jsonl");
        let fat = "y".repeat(120 * 1024);
        std::fs::write(
            &path,
            format!(
                "{{\"type\":\"meta\",\"blob\":\"{fat}\"}}\n{{\"type\":\"noise\"}}\n{{\"type\":\"target\",\"model\":\"m1\"}}\n{{\"type\":\"after\"}}\n"
            ),
        )
        .unwrap();
        let lines = head_lines(&path, 10, 1024 * 1024, |v| v["type"] == "target").unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines.last().unwrap()["model"], "m1");
    }

    #[test]
    fn garbage_lines_are_skipped_not_fatal() {
        let dir = std::env::temp_dir().join("portal-jsonl-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("garbage.jsonl");
        std::fs::write(&path, "{\"ok\":1}\nnot json at all\n{\"ok\":2}\n").unwrap();
        let p = peek(&path, DEFAULT_WINDOW).unwrap();
        assert_eq!(p.head.len(), 2);
        assert_eq!(p.exact_line_count, Some(3));
    }
}
