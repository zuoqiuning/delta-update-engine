use std::io::Read;
use std::path::Path;

use crate::error::{EngineError, PatchError};
use crate::limits::Limits;

use super::container::{Codec, PatchHeader, PATCH_HEADER_LEN};

/// Apply a `.tdp` patch to a materialized staging file.
///
/// The payload is decompressed with the container codec and then applied with
/// qbsdiff. The caller verifies the final target hash and size.
pub fn apply_patch(
    source: &Path,
    patch: &Path,
    dest: &Path,
    header: &PatchHeader,
    limits: &Limits,
) -> Result<(), EngineError> {
    if header.target_size > limits.max_full_bytes {
        return Err(PatchError::FullTooLarge(format!(
            "target size {} exceeds the full object limit",
            header.target_size
        ))
        .into());
    }

    let patch_bytes = std::fs::read(patch)
        .map_err(|err| EngineError::Internal(format!("read patch {}: {err}", patch.display())))?;
    if patch_bytes.len() < PATCH_HEADER_LEN {
        return Err(PatchError::FormatMismatch("patch is truncated".into()).into());
    }
    let decoded = decode_payload(&patch_bytes[PATCH_HEADER_LEN..], header.codec, limits)?;

    let source_bytes = std::fs::read(source)
        .map_err(|err| EngineError::Internal(format!("read source {}: {err}", source.display())))?;

    let mut target = std::fs::File::create(dest)
        .map_err(|err| EngineError::PermissionDenied(format!("{}: {err}", dest.display())))?;
    let written = qbsdiff::Bspatch::new(&decoded)
        .map_err(|err| PatchError::FormatMismatch(format!("invalid bsdiff payload: {err}")))?
        .apply(&source_bytes, &mut target)
        .map_err(|err| PatchError::ApplyFailed(err.to_string()))?;
    target
        .sync_all()
        .map_err(|err| EngineError::PermissionDenied(format!("{}: {err}", dest.display())))?;

    if written != header.target_size {
        return Err(PatchError::DecompressionLimit(format!(
            "target is {written} bytes, manifest declares {}",
            header.target_size
        ))
        .into());
    }
    Ok(())
}

fn decode_payload(payload: &[u8], codec: Codec, limits: &Limits) -> Result<Vec<u8>, EngineError> {
    match codec {
        Codec::None => {
            if payload.len() as u64 > limits.max_patch_file_size {
                return Err(PatchError::TooLarge(format!(
                    "decoded payload is {} bytes",
                    payload.len()
                ))
                .into());
            }
            Ok(payload.to_vec())
        }
        Codec::Zstd => {
            let decoder = zstd::stream::read::Decoder::new(payload).map_err(|err| {
                PatchError::FormatMismatch(format!("invalid zstd payload: {err}"))
            })?;
            read_limited(decoder, limits.max_patch_file_size)
        }
        Codec::Lzma2 => Err(PatchError::ApplyFailed(
            "lzma2 patches are not supported by this client".into(),
        )
        .into()),
    }
}

fn read_limited<R: Read>(reader: R, limit: u64) -> Result<Vec<u8>, EngineError> {
    let mut out = Vec::new();
    reader
        .take(limit.saturating_add(1))
        .read_to_end(&mut out)
        .map_err(|err| PatchError::FormatMismatch(format!("decompress failed: {err}")))?;
    if out.len() as u64 > limit {
        return Err(PatchError::DecompressionLimit(format!(
            "decoded payload exceeds the {limit} byte limit"
        ))
        .into());
    }
    Ok(out)
}
