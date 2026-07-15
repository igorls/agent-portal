use std::fs::File;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const SIMPLE_CACHE_HEADER: u64 = 24;
const MAX_KEY_LEN: usize = 16 * 1024;
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub key: String,
    pub body: Vec<u8>,
    pub path: PathBuf,
}

pub fn relevant_entries(root: &Path) -> Vec<CacheEntry> {
    let dir = root.join("Cache").join("Cache_Data");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    entries
        .flatten()
        .filter_map(|entry| read_entry(&entry.path()).ok().flatten())
        .filter(|entry| {
            entry.key.contains("claude.ai/v1/code/sessions/")
                || entry.key.contains("claude.ai/v1/code/sessions?")
        })
        .collect()
}

fn read_entry(path: &Path) -> std::io::Result<Option<CacheEntry>> {
    if !path.is_file()
        || !path
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| name.ends_with("_0"))
    {
        return Ok(None);
    }
    let mut file = File::open(path)?;
    let mut header = [0_u8; SIMPLE_CACHE_HEADER as usize];
    if file.read_exact(&mut header).is_err() {
        return Ok(None);
    }
    let key_len = u32::from_le_bytes(header[12..16].try_into().unwrap()) as usize;
    if key_len == 0 || key_len > MAX_KEY_LEN {
        return Ok(None);
    }
    let mut key = vec![0_u8; key_len];
    file.read_exact(&mut key)?;
    let Ok(key) = String::from_utf8(key) else {
        return Ok(None);
    };
    if !key.contains("claude.ai/v1/code/sessions") {
        return Ok(None);
    }

    file.seek(SeekFrom::Start(SIMPLE_CACHE_HEADER + key_len as u64))?;
    let mut body = Vec::new();
    file.read_to_end(&mut body)?;
    if body.starts_with(&ZSTD_MAGIC) {
        body = zstd::stream::decode_all(Cursor::new(body))?;
    }
    Ok(Some(CacheEntry {
        key,
        body,
        path: path.to_path_buf(),
    }))
}

pub fn sse_json(body: &[u8]) -> Vec<serde_json::Value> {
    String::from_utf8_lossy(body)
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn reads_uncompressed_and_zstd_simple_cache_entries() {
        let dir =
            std::env::temp_dir().join(format!("portal-cowork-cache-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let key = "1/0/https://claude.ai/v1/code/sessions/cse_test/events?limit=500";
        let body = br#"{"data":[]}"#;
        for (name, encoded) in [
            ("plain_0", body.to_vec()),
            (
                "zstd_0",
                zstd::stream::encode_all(Cursor::new(body), 1).unwrap(),
            ),
        ] {
            let path = dir.join(name);
            let mut file = File::create(&path).unwrap();
            let mut header = [0_u8; SIMPLE_CACHE_HEADER as usize];
            header[12..16].copy_from_slice(&(key.len() as u32).to_le_bytes());
            file.write_all(&header).unwrap();
            file.write_all(key.as_bytes()).unwrap();
            file.write_all(&encoded).unwrap();
            let parsed = read_entry(&path).unwrap().unwrap();
            assert_eq!(parsed.key, key);
            assert_eq!(parsed.body, body);
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn parses_sse_data_lines_and_ignores_keepalives() {
        let values = sse_json(b":keepalive\n\ndata: {\"event_type\":\"user\"}\n\n");
        assert_eq!(values.len(), 1);
        assert_eq!(values[0]["event_type"], "user");
    }
}
