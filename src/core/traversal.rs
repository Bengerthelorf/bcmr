use crate::core::error::BcmrError;
use std::path::Path;
use walkdir::{DirEntry, WalkDir};

/// Match a selected source spelling and its lexical source ancestors.
/// Destination names and canonical link targets are deliberately not matched.
pub fn is_copy_excluded(root: &Path, path: &Path, excludes: &[regex::Regex]) -> bool {
    let relative = path.strip_prefix(root).unwrap_or(path);
    if root
        .ancestors()
        .filter(|ancestor| !ancestor.as_os_str().is_empty())
        .any(|ancestor| is_excluded(ancestor, excludes))
    {
        return true;
    }
    let mut selected = std::path::PathBuf::new();
    for component in relative.components() {
        selected.push(component.as_os_str());
        if is_excluded(&selected, excludes) || is_excluded(&root.join(&selected), excludes) {
            return true;
        }
    }
    false
}

/// Remote paths always use '/', including on Windows clients.
pub fn is_remote_entry_excluded(root: &str, relative: &str, excludes: &[regex::Regex]) -> bool {
    let matches = |name: &str| excludes.iter().any(|pattern| pattern.is_match(name));
    let mut ancestor = root.trim_end_matches('/');
    while !ancestor.is_empty() {
        if matches(ancestor) {
            return true;
        }
        match ancestor.rsplit_once('/') {
            Some((parent, _)) => ancestor = parent,
            None => break,
        }
    }
    let mut selected = String::new();
    for component in relative.split('/').filter(|part| !part.is_empty()) {
        if !selected.is_empty() {
            selected.push('/');
        }
        selected.push_str(component);
        let full = format!("{}/{}", root.trim_end_matches('/'), selected);
        if matches(&selected) || matches(&full) {
            return true;
        }
    }
    false
}

/// Inventory selected copy entries before yielding any of them. Prune before
/// reading link targets, then follow only selected links. A global visited set
/// would incorrectly reject two ordinary aliases for the same directory, so
/// physical directory identities are compared only with the active ancestry.
pub fn copy_walk(
    root: &Path,
    recursive: bool,
    min_depth: usize,
    no_deref: bool,
    excludes: &[regex::Regex],
) -> impl Iterator<Item = Result<DirEntry, BcmrError>> {
    let collect = || -> Result<Vec<DirEntry>, BcmrError> {
        let mut entries = Vec::new();
        let mut pending = vec![(root.to_path_buf(), 0usize, Vec::<std::path::PathBuf>::new())];
        while let Some((path, depth, mut ancestors)) = pending.pop() {
            if is_copy_excluded(root, &path, excludes) {
                continue;
            }
            // WalkDir supplies a DirEntry whose metadata agrees with our link
            // policy. Disable its special root-following behavior for -P,
            // which must also accept dangling root symlinks.
            let entry = WalkDir::new(&path)
                .follow_root_links(false)
                .follow_links(!no_deref)
                .max_depth(0)
                .into_iter()
                .next()
                .ok_or_else(|| BcmrError::SourceNotFound(path.clone()))??;
            let is_dir = entry.file_type().is_dir();
            if is_dir {
                let physical = path.canonicalize()?;
                if ancestors.contains(&physical) {
                    return Err(BcmrError::InvalidInput(format!(
                        "symbolic-link directory cycle at '{}'",
                        path.display()
                    )));
                }
                ancestors.push(physical);
                if recursive || depth == 0 {
                    let mut children = std::fs::read_dir(&path)?
                        .map(|child| child.map(|child| child.path()))
                        .collect::<std::io::Result<Vec<_>>>()?;
                    children.sort();
                    for child in children.into_iter().rev() {
                        pending.push((child, depth + 1, ancestors.clone()));
                    }
                }
            }
            if depth >= min_depth {
                entries.push(entry);
            }
        }
        Ok(entries)
    };
    match collect() {
        Ok(entries) => entries.into_iter().map(Ok).collect::<Vec<_>>(),
        Err(error) => vec![Err(error)],
    }
    .into_iter()
}

pub fn is_excluded(path: &Path, excludes: &[regex::Regex]) -> bool {
    let path_str = path.to_string_lossy();
    excludes.iter().any(|re| re.is_match(&path_str))
}

pub fn walk(
    root: &Path,
    recursive: bool,
    contents_first: bool,
    min_depth: usize,
    excludes: &[regex::Regex],
) -> impl Iterator<Item = walkdir::Result<DirEntry>> {
    let mut walker = WalkDir::new(root).follow_root_links(false);

    if min_depth > 0 {
        walker = walker.min_depth(min_depth);
    }

    if contents_first {
        walker = walker.contents_first(true);
    }

    if !recursive {
        walker = walker.max_depth(1);
    }

    let excludes = excludes.to_vec();

    walker
        .into_iter()
        .filter_entry(move |e| !is_excluded(e.path(), &excludes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_excluded_matching() {
        let excludes = vec![regex::Regex::new(r"\.log$").unwrap()];
        assert!(is_excluded(Path::new("file.log"), &excludes));
        assert!(!is_excluded(Path::new("file.txt"), &excludes));
    }

    #[test]
    fn test_is_excluded_empty_patterns() {
        let excludes: Vec<regex::Regex> = vec![];
        assert!(!is_excluded(Path::new("anything.txt"), &excludes));
    }

    #[test]
    fn test_is_excluded_multiple_patterns() {
        let excludes = vec![
            regex::Regex::new(r"\.log$").unwrap(),
            regex::Regex::new(r"\.tmp$").unwrap(),
        ];
        assert!(is_excluded(Path::new("file.log"), &excludes));
        assert!(is_excluded(Path::new("file.tmp"), &excludes));
        assert!(!is_excluded(Path::new("file.txt"), &excludes));
    }

    #[test]
    fn test_walk_flat_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        std::fs::write(dir.path().join("b.txt"), "b").unwrap();

        let entries: Vec<_> = walk(dir.path(), false, false, 1, &[])
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn test_walk_recursive() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("b.txt"), "b").unwrap();

        let entries: Vec<_> = walk(dir.path(), true, false, 1, &[])
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn test_walk_with_exclude() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("keep.txt"), "keep").unwrap();
        std::fs::write(dir.path().join("skip.log"), "skip").unwrap();

        let excludes = vec![regex::Regex::new(r"\.log$").unwrap()];
        let entries: Vec<_> = walk(dir.path(), false, false, 1, &excludes)
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].path().to_string_lossy().contains("keep.txt"));
    }

    #[test]
    fn test_walk_contents_first() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("file.txt"), "x").unwrap();

        let entries: Vec<_> = walk(dir.path(), true, true, 1, &[])
            .filter_map(|e| e.ok())
            .collect();
        let file_idx = entries.iter().position(|e| e.path().is_file()).unwrap();
        let dir_idx = entries.iter().position(|e| e.path() == sub).unwrap();
        assert!(file_idx < dir_idx);
    }
}
