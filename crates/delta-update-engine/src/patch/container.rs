use crate::error::{EngineError, PatchError};

pub const PATCH_MAGIC: [u8; 4] = *b"TDP1";
pub const PATCH_HEADER_LEN: usize = 82;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[non_exhaustive]
pub enum PatchFormat {
    V1 = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[non_exhaustive]
pub enum Codec {
    None = 0,
    Zstd = 1,
    Lzma2 = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatchHeader {
    pub format: u16,
    pub strategy_id: u16,
    pub codec: Codec,
    pub target_size: u64,
    pub source_sha256: [u8; 32],
    pub target_sha256: [u8; 32],
}

impl PatchHeader {
    pub fn parse(bytes: &[u8]) -> Result<(Self, &[u8]), EngineError> {
        if bytes.len() < PATCH_HEADER_LEN {
            return Err(PatchError::FormatMismatch("patch header is truncated".into()).into());
        }
        if bytes[..4] != PATCH_MAGIC {
            return Err(PatchError::FormatMismatch("invalid patch magic".into()).into());
        }

        let format = u16::from_le_bytes([bytes[4], bytes[5]]);
        if format != PatchFormat::V1 as u16 {
            return Err(
                PatchError::FormatMismatch(format!("unsupported patch format: {format}")).into(),
            );
        }
        let strategy_id = u16::from_le_bytes([bytes[6], bytes[7]]);
        let codec = match bytes[8] {
            0 => Codec::None,
            1 => Codec::Zstd,
            2 => Codec::Lzma2,
            value => {
                return Err(
                    PatchError::FormatMismatch(format!("unsupported patch codec: {value}")).into(),
                )
            }
        };
        if bytes[9] != 0 {
            return Err(PatchError::FormatMismatch(format!(
                "reserved flags must be zero: {}",
                bytes[9]
            ))
            .into());
        }

        let mut target_size = [0_u8; 8];
        target_size.copy_from_slice(&bytes[10..18]);
        let mut source_sha256 = [0_u8; 32];
        source_sha256.copy_from_slice(&bytes[18..50]);
        let mut target_sha256 = [0_u8; 32];
        target_sha256.copy_from_slice(&bytes[50..82]);

        Ok((
            Self {
                format,
                strategy_id,
                codec,
                target_size: u64::from_le_bytes(target_size),
                source_sha256,
                target_sha256,
            },
            &bytes[PATCH_HEADER_LEN..],
        ))
    }

    pub fn encode(&self) -> [u8; PATCH_HEADER_LEN] {
        let mut out = [0_u8; PATCH_HEADER_LEN];
        out[..4].copy_from_slice(&PATCH_MAGIC);
        out[4..6].copy_from_slice(&self.format.to_le_bytes());
        out[6..8].copy_from_slice(&self.strategy_id.to_le_bytes());
        out[8] = self.codec as u8;
        out[10..18].copy_from_slice(&self.target_size.to_le_bytes());
        out[18..50].copy_from_slice(&self.source_sha256);
        out[50..82].copy_from_slice(&self.target_sha256);
        out
    }
}
