use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context;
use delta_update_engine::patch::container::{Codec, PatchHeader};
use delta_update_engine::Strategy;
use rayon::prelude::*;

use crate::pipeline::plan::{ChangeKind, PlannedFile};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    Incompressible,
    TooLarge,
    Ratio,
}

#[derive(Debug, Clone)]
pub enum PatchAttempt {
    Candidate(PatchCandidate),
    Skipped(SkipReason),
}

#[derive(Debug, Clone)]
pub struct PatchCandidate {
    pub bytes: Vec<u8>,
    pub full_size: u64,
}

impl PatchCandidate {
    pub fn ratio(&self) -> f64 {
        if self.full_size == 0 {
            1.0
        } else {
            self.bytes.len() as f64 / self.full_size as f64
        }
    }
}

/// Memory held by one patch job, relative to the larger side of the file.
/// qbsdiff keeps source, target, a four-byte suffix array per source byte and
/// the delta; 6x covers the suffix array plus diff/extra buffers.
pub const PATCH_MEMORY_FACTOR: u64 = 6;

/// Files that will actually reach qbsdiff, largest first.
///
/// Incompressible extensions and files above `max_file_size` are skipped before
/// any of them is read, so counting them in the peak estimate would
/// over-provision memory (and under-provision nothing).
pub fn patch_input_sizes<'a>(
    plan: &'a [PlannedFile],
    from_scan: &crate::pipeline::scan::ScanResult,
    to_scan: &crate::pipeline::scan::ScanResult,
    max_file_size: u64,
) -> Vec<(&'a str, u64)> {
    let mut sizes: Vec<(&str, u64)> = plan
        .iter()
        .filter(|file| file.kind == ChangeKind::Modified && !is_incompressible(&file.path))
        .filter_map(|file| {
            let source = from_scan.get(&file.path)?;
            let target = to_scan.get(&file.path)?;
            if target.size == 0 || source.size > max_file_size || target.size > max_file_size {
                return None;
            }
            Some((file.path.as_str(), source.size.max(target.size)))
        })
        .collect();
    sizes.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    sizes
}

/// Peak memory of the whole pool.
///
/// Each job processes one file at a time, so a slot's peak is its largest
/// input. The pool peak is therefore the sum of the `jobs` largest inputs, not
/// `jobs × patch_max_file_size`: a release with one big file and many small
/// ones should not be estimated as if every slot held a big file.
pub fn estimated_peak_bytes(sizes: &[(&str, u64)], jobs: usize) -> u64 {
    let jobs = jobs.max(1);
    sizes
        .iter()
        .take(jobs)
        .map(|(_, size)| *size)
        .fold(0, u64::saturating_add)
        .saturating_mul(PATCH_MEMORY_FACTOR)
}

/// Generate `.tdp` candidates for every modified file that is worth patching.
/// Files that are already compressed or too large are reported as skipped so
/// `--require-delta` and verbose output stay accurate.
pub fn generate(
    from_root: &Path,
    to_root: &Path,
    plan: &[PlannedFile],
    from_scan: &crate::pipeline::scan::ScanResult,
    to_scan: &crate::pipeline::scan::ScanResult,
    max_file_size: u64,
    max_ratio: f64,
) -> anyhow::Result<BTreeMap<String, PatchAttempt>> {
    let candidates: Vec<(String, anyhow::Result<PatchAttempt>)> = plan
        .par_iter()
        .filter(|file| file.kind == ChangeKind::Modified)
        .map(|file| {
            let result = generate_one(
                file,
                from_root,
                to_root,
                from_scan,
                to_scan,
                max_file_size,
                max_ratio,
            );
            (file.path.clone(), result)
        })
        .collect();

    let mut results = BTreeMap::new();
    for (path, result) in candidates {
        results.insert(path, result?);
    }
    Ok(results)
}

fn generate_one(
    file: &PlannedFile,
    from_root: &Path,
    to_root: &Path,
    from_scan: &crate::pipeline::scan::ScanResult,
    to_scan: &crate::pipeline::scan::ScanResult,
    max_file_size: u64,
    max_ratio: f64,
) -> anyhow::Result<PatchAttempt> {
    let target = to_scan
        .get(&file.path)
        .with_context(|| format!("missing target entry for {}", file.path))?;
    let source = from_scan
        .get(&file.path)
        .with_context(|| format!("missing source entry for {}", file.path))?;

    if is_incompressible(&file.path) {
        return Ok(PatchAttempt::Skipped(SkipReason::Incompressible));
    }
    if source.size > max_file_size || target.size > max_file_size || target.size == 0 {
        return Ok(PatchAttempt::Skipped(SkipReason::TooLarge));
    }

    let old_path = from_root.join(Path::new(&file.path));
    let new_path = to_root.join(Path::new(&file.path));
    let old = std::fs::read(&old_path).with_context(|| format!("read {}", old_path.display()))?;
    let new = std::fs::read(&new_path).with_context(|| format!("read {}", new_path.display()))?;

    let mut bsdiff = Vec::new();
    qbsdiff::Bsdiff::new(&old, &new)
        .compare(&mut bsdiff)
        .context("generate bsdiff payload")?;
    let payload = zstd::encode_all(bsdiff.as_slice(), 3).context("compress patch")?;

    let header = PatchHeader {
        format: 1,
        strategy_id: Strategy::QbsdiffZstd.patch_id().unwrap(),
        codec: Codec::Zstd,
        target_size: new.len() as u64,
        source_sha256: decode_sha(&source.sha256)?,
        target_sha256: decode_sha(&target.sha256)?,
    };
    let mut bytes = header.encode().to_vec();
    bytes.extend_from_slice(&payload);

    let full_size = new.len() as u64;
    if bytes.len() as f64 >= full_size as f64 * max_ratio {
        return Ok(PatchAttempt::Skipped(SkipReason::Ratio));
    }

    Ok(PatchAttempt::Candidate(PatchCandidate { bytes, full_size }))
}

/// Formats that are already compressed; diffing them produces patches that are
/// almost always larger than the full object.
pub fn is_incompressible(path: &str) -> bool {
    const EXTENSIONS: &[&str] = &[
        "png", "jpg", "jpeg", "gif", "webp", "ico", "zip", "gz", "tgz", "bz2", "xz", "7z", "rar",
        "jar", "apk", "dmg", "mp3", "mp4", "m4a", "ogg", "opus", "webm", "woff", "woff2",
    ];
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            EXTENSIONS
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
}

fn decode_sha(value: &str) -> anyhow::Result<[u8; 32]> {
    let decoded = hex::decode(value).context("decode sha256")?;
    let bytes: [u8; 32] = decoded
        .try_into()
        .map_err(|_| anyhow::anyhow!("sha256 must be 32 bytes"))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ignore::DeltaIgnore;
    use crate::pipeline::plan::plan;
    use crate::pipeline::scan::scan;

    #[test]
    fn peak_is_the_sum_of_the_largest_inputs_per_slot() {
        let sizes = [("a", 100_u64), ("b", 90), ("c", 40), ("d", 1)];
        assert_eq!(
            estimated_peak_bytes(&sizes, 2),
            (100 + 90) * PATCH_MEMORY_FACTOR
        );
        assert_eq!(estimated_peak_bytes(&sizes, 1), 100 * PATCH_MEMORY_FACTOR);
        assert_eq!(estimated_peak_bytes(&sizes, 8), 231 * PATCH_MEMORY_FACTOR);
        assert_eq!(estimated_peak_bytes(&[], 4), 0);
    }

    #[test]
    fn patch_inputs_skip_files_that_never_reach_qbsdiff() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        let to = dir.path().join("to");
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        // Oversized: above the 1 KiB limit, never read by qbsdiff.
        std::fs::write(from.join("big.bin"), vec![b'a'; 2048]).unwrap();
        std::fs::write(to.join("big.bin"), vec![b'a'; 2048]).unwrap();
        // Incompressible extension.
        std::fs::write(from.join("icon.png"), b"old-icon").unwrap();
        std::fs::write(to.join("icon.png"), b"new-icon").unwrap();
        // Eligible.
        std::fs::write(from.join("small.bin"), b"aaaa").unwrap();
        std::fs::write(to.join("small.bin"), b"bbbb").unwrap();

        let ignore = DeltaIgnore::default();
        let from_scan = scan(&from, &ignore).unwrap();
        let to_scan = scan(&to, &ignore).unwrap();
        let planned = plan(Some(&from_scan), &to_scan);
        let inputs = patch_input_sizes(&planned, &from_scan, &to_scan, 1024);
        assert_eq!(
            inputs.iter().map(|(path, _)| *path).collect::<Vec<_>>(),
            ["small.bin"]
        );
    }

    #[test]
    fn generates_a_container_that_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        let to = dir.path().join("to");
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        let mut old = vec![b'a'; 8 * 1024];
        let mut new = old.clone();
        new[100..110].copy_from_slice(b"changed123");
        old.extend_from_slice(&vec![b'b'; 1024]);
        new.extend_from_slice(&vec![b'b'; 1024]);
        std::fs::write(from.join("app.bin"), &old).unwrap();
        std::fs::write(to.join("app.bin"), &new).unwrap();

        let ignore = DeltaIgnore::default();
        let from_scan = scan(&from, &ignore).unwrap();
        let to_scan = scan(&to, &ignore).unwrap();
        let planned = plan(Some(&from_scan), &to_scan);
        let results =
            generate(&from, &to, &planned, &from_scan, &to_scan, 1024 * 1024, 1.0).unwrap();
        let PatchAttempt::Candidate(candidate) = &results["app.bin"] else {
            panic!("expected a patch candidate");
        };
        let (header, _) = PatchHeader::parse(&candidate.bytes).unwrap();
        assert_eq!(header.target_size, new.len() as u64);
    }

    #[test]
    fn skips_incompressible_extensions() {
        assert!(is_incompressible("resources/icon.png"));
        assert!(!is_incompressible("bin/app.exe"));
    }

    #[test]
    fn falls_back_when_the_patch_is_too_large() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        let to = dir.path().join("to");
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(from.join("app.bin"), b"aaaa").unwrap();
        std::fs::write(to.join("app.bin"), b"bbbb").unwrap();

        let ignore = DeltaIgnore::default();
        let from_scan = scan(&from, &ignore).unwrap();
        let to_scan = scan(&to, &ignore).unwrap();
        let planned = plan(Some(&from_scan), &to_scan);
        let results = generate(
            &from,
            &to,
            &planned,
            &from_scan,
            &to_scan,
            1024 * 1024,
            0.01,
        )
        .unwrap();
        assert!(matches!(
            results["app.bin"],
            PatchAttempt::Skipped(SkipReason::Ratio)
        ));
    }
}
