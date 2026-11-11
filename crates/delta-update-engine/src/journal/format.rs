use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::crypto::sha256_bytes;
use crate::error::EngineError;

pub const JOURNAL_FORMAT: u16 = 1;
pub const JOURNAL_MAGIC: &[u8; 4] = b"TDJ1";
pub const JOURNAL_MAX_BYTES: usize = 32 * 1024 * 1024;
pub const JOURNAL_MAX_OPS: usize = 100_000;
pub const JOURNAL_MAX_STRING: usize = 1024;
const CHECKSUM_LEN: usize = 32;
const HEADER_LEN: usize = 4 + 2 + 4 + 4 + 16;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum FileOp {
    Backup {
        src: String,
        dst: String,
    },
    Place {
        staging_src: String,
        dst: String,
        sha256: String,
        size: u64,
    },
    Trash {
        src: String,
        trash_dst: String,
    },
    Restore {
        backup_src: String,
        dst: String,
        sha256: String,
        size: u64,
    },
    RemoveTrash {
        trash_path: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Journal {
    pub format: u16,
    pub commit_id: String,
    pub version: String,
    pub from_version: String,
    pub ops: Vec<FileOp>,
}

impl Journal {
    pub fn new(
        commit_id: impl Into<String>,
        version: impl Into<String>,
        from_version: impl Into<String>,
        ops: Vec<FileOp>,
    ) -> Self {
        Self {
            format: JOURNAL_FORMAT,
            commit_id: commit_id.into(),
            version: version.into(),
            from_version: from_version.into(),
            ops,
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, EngineError> {
        if self.format != JOURNAL_FORMAT {
            return Err(corrupt(format!("unsupported format {}", self.format)));
        }
        if self.ops.len() > JOURNAL_MAX_OPS {
            return Err(corrupt("too many operations"));
        }
        let commit_id = Uuid::parse_str(&self.commit_id)
            .map_err(|err| corrupt(format!("invalid commit_id: {err}")))?;

        let mut payload = Vec::new();
        write_str(&mut payload, &self.version)?;
        write_str(&mut payload, &self.from_version)?;
        for op in &self.ops {
            write_op(&mut payload, op)?;
        }
        if payload.len() > JOURNAL_MAX_BYTES {
            return Err(corrupt("payload exceeds the journal size limit"));
        }

        let mut out = Vec::with_capacity(HEADER_LEN + payload.len() + CHECKSUM_LEN);
        out.extend_from_slice(JOURNAL_MAGIC);
        out.extend_from_slice(&self.format.to_le_bytes());
        out.extend_from_slice(&(self.ops.len() as u32).to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(commit_id.as_bytes());
        out.extend_from_slice(&payload);
        let checksum =
            hex::decode(sha256_bytes(&out)).map_err(|err| corrupt(format!("checksum: {err}")))?;
        out.extend_from_slice(&checksum);
        if out.len() > JOURNAL_MAX_BYTES {
            return Err(corrupt("journal exceeds the size limit"));
        }
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, EngineError> {
        if bytes.len() > JOURNAL_MAX_BYTES {
            return Err(corrupt("journal exceeds the size limit"));
        }
        if bytes.len() < HEADER_LEN + CHECKSUM_LEN {
            return Err(corrupt("journal is truncated"));
        }
        if &bytes[..4] != JOURNAL_MAGIC {
            return Err(corrupt("invalid journal magic"));
        }
        let format = u16::from_le_bytes([bytes[4], bytes[5]]);
        if format != JOURNAL_FORMAT {
            return Err(corrupt(format!("unsupported format {format}")));
        }
        let op_count = u32::from_le_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]) as usize;
        let payload_len = u32::from_le_bytes([bytes[10], bytes[11], bytes[12], bytes[13]]) as usize;
        if op_count > JOURNAL_MAX_OPS {
            return Err(corrupt("too many operations"));
        }
        let expected = HEADER_LEN
            .checked_add(payload_len)
            .and_then(|value| value.checked_add(CHECKSUM_LEN))
            .ok_or_else(|| corrupt("journal length overflow"))?;
        if bytes.len() != expected {
            return Err(corrupt("journal length does not match the header"));
        }

        let body = &bytes[..HEADER_LEN + payload_len];
        let expected_checksum =
            hex::decode(sha256_bytes(body)).map_err(|err| corrupt(format!("checksum: {err}")))?;
        if bytes[HEADER_LEN + payload_len..] != expected_checksum {
            return Err(corrupt("journal checksum mismatch"));
        }
        let commit_id = Uuid::from_slice(&bytes[14..30])
            .map_err(|err| corrupt(format!("invalid commit_id: {err}")))?
            .to_string();

        let payload = &bytes[HEADER_LEN..HEADER_LEN + payload_len];
        let mut reader = Reader::new(payload);
        let version = reader.string()?;
        let from_version = reader.string()?;
        let mut ops = Vec::with_capacity(op_count);
        for _ in 0..op_count {
            ops.push(reader.op()?);
        }
        if !reader.is_empty() {
            return Err(corrupt("trailing bytes in journal payload"));
        }

        Ok(Self {
            format,
            commit_id,
            version,
            from_version,
            ops,
        })
    }

    pub fn write_atomic(&self, path: &std::path::Path) -> Result<(), EngineError> {
        let bytes = self.encode()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| EngineError::JournalCorrupt(format!("create journal dir: {err}")))?;
        }
        crate::fs::atomic::write_atomic(path, &bytes)
            .map_err(|err| EngineError::JournalCorrupt(format!("write journal: {err}")))?;
        if let Some(parent) = path.parent() {
            let _ = crate::fs::atomic::sync_dir(parent);
        }
        Ok(())
    }
}

fn write_str(out: &mut Vec<u8>, value: &str) -> Result<(), EngineError> {
    if value.len() > JOURNAL_MAX_STRING {
        return Err(corrupt("string field is too long"));
    }
    out.extend_from_slice(&(value.len() as u16).to_le_bytes());
    out.extend_from_slice(value.as_bytes());
    Ok(())
}

fn write_sha(out: &mut Vec<u8>, value: &str) -> Result<(), EngineError> {
    let bytes = hex::decode(value).map_err(|err| corrupt(format!("sha256: {err}")))?;
    if bytes.len() != 32 {
        return Err(corrupt("sha256 must be 32 bytes"));
    }
    out.extend_from_slice(&bytes);
    Ok(())
}

fn write_op(out: &mut Vec<u8>, op: &FileOp) -> Result<(), EngineError> {
    match op {
        FileOp::Backup { src, dst } => {
            out.push(0x01);
            write_str(out, src)?;
            write_str(out, dst)?;
        }
        FileOp::Place {
            staging_src,
            dst,
            sha256,
            size,
        } => {
            out.push(0x02);
            write_str(out, staging_src)?;
            write_str(out, dst)?;
            write_sha(out, sha256)?;
            out.extend_from_slice(&size.to_le_bytes());
        }
        FileOp::Trash { src, trash_dst } => {
            out.push(0x03);
            write_str(out, src)?;
            write_str(out, trash_dst)?;
        }
        FileOp::Restore {
            backup_src,
            dst,
            sha256,
            size,
        } => {
            out.push(0x04);
            write_str(out, backup_src)?;
            write_str(out, dst)?;
            write_sha(out, sha256)?;
            out.extend_from_slice(&size.to_le_bytes());
        }
        FileOp::RemoveTrash { trash_path } => {
            out.push(0x05);
            write_str(out, trash_path)?;
        }
    }
    Ok(())
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], EngineError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or_else(|| corrupt("journal offset overflow"))?;
        if end > self.bytes.len() {
            return Err(corrupt("journal payload is truncated"));
        }
        let slice = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(slice)
    }

    fn u16(&mut self) -> Result<u16, EngineError> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u64(&mut self) -> Result<u64, EngineError> {
        let bytes = self.take(8)?;
        let mut value = [0_u8; 8];
        value.copy_from_slice(bytes);
        Ok(u64::from_le_bytes(value))
    }

    fn string(&mut self) -> Result<String, EngineError> {
        let len = self.u16()? as usize;
        if len > JOURNAL_MAX_STRING {
            return Err(corrupt("string field is too long"));
        }
        let bytes = self.take(len)?;
        std::str::from_utf8(bytes)
            .map(str::to_string)
            .map_err(|err| corrupt(format!("invalid UTF-8: {err}")))
    }

    fn sha(&mut self) -> Result<String, EngineError> {
        Ok(hex::encode(self.take(32)?))
    }

    fn op(&mut self) -> Result<FileOp, EngineError> {
        let tag = self.take(1)?[0];
        Ok(match tag {
            0x01 => FileOp::Backup {
                src: self.string()?,
                dst: self.string()?,
            },
            0x02 => FileOp::Place {
                staging_src: self.string()?,
                dst: self.string()?,
                sha256: self.sha()?,
                size: self.u64()?,
            },
            0x03 => FileOp::Trash {
                src: self.string()?,
                trash_dst: self.string()?,
            },
            0x04 => FileOp::Restore {
                backup_src: self.string()?,
                dst: self.string()?,
                sha256: self.sha()?,
                size: self.u64()?,
            },
            0x05 => FileOp::RemoveTrash {
                trash_path: self.string()?,
            },
            value => return Err(corrupt(format!("unknown operation type {value}"))),
        })
    }

    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

fn corrupt(reason: impl Into<String>) -> EngineError {
    EngineError::JournalCorrupt(reason.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_ops() -> Vec<FileOp> {
        vec![
            FileOp::Backup {
                src: "App.exe".into(),
                dst: ".old/abc/App.exe".into(),
            },
            FileOp::Place {
                staging_src: "App.exe".into(),
                dst: "App.exe".into(),
                sha256: "a".repeat(64),
                size: 42,
            },
            FileOp::Trash {
                src: "old.dll".into(),
                trash_dst: "old.dll".into(),
            },
            FileOp::Restore {
                backup_src: "old/App.exe".into(),
                dst: "App.exe".into(),
                sha256: "b".repeat(64),
                size: 7,
            },
            FileOp::RemoveTrash {
                trash_path: "old.dll".into(),
            },
        ]
    }

    #[test]
    fn round_trips_every_operation() {
        let journal = Journal::new(Uuid::new_v4().to_string(), "2.0.0", "1.0.0", all_ops());
        let bytes = journal.encode().unwrap();
        assert_eq!(Journal::decode(&bytes).unwrap(), journal);
    }

    #[test]
    fn rejects_tampered_checksum() {
        let journal = Journal::new(Uuid::new_v4().to_string(), "2.0.0", "1.0.0", all_ops());
        let mut bytes = journal.encode().unwrap();
        let len = bytes.len();
        bytes[len - 1] ^= 0xff;
        let error = Journal::decode(&bytes).unwrap_err();
        assert_eq!(error.code(), "E_JOURNAL_CORRUPT");
    }

    #[test]
    fn rejects_trailing_bytes() {
        let journal = Journal::new(Uuid::new_v4().to_string(), "2.0.0", "1.0.0", vec![]);
        let mut bytes = journal.encode().unwrap();
        bytes.push(0);
        assert!(Journal::decode(&bytes).is_err());
    }
}
