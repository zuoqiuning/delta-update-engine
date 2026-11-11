use crate::pipeline::scan::ScanResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChangeKind {
    Unchanged,
    Modified,
    Added,
}

#[derive(Debug, Clone)]
pub struct PlannedFile {
    pub path: String,
    pub kind: ChangeKind,
}

/// Compare the previous and next scans. Patch generation consumes this plan.
pub fn plan(from: Option<&ScanResult>, to: &ScanResult) -> Vec<PlannedFile> {
    to.iter()
        .map(|(path, file)| {
            let kind = match from.and_then(|from| from.get(path)) {
                Some(previous) if previous.sha256 == file.sha256 => ChangeKind::Unchanged,
                Some(_) => ChangeKind::Modified,
                None => ChangeKind::Added,
            };
            PlannedFile {
                path: path.clone(),
                kind,
            }
        })
        .collect()
}
