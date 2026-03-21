use walkdir::{DirEntry, WalkDir};
use std::path::Path;

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
    let mut walker = WalkDir::new(root);
    
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

    walker.into_iter().filter_entry(move |e| {
        !is_excluded(e.path(), &excludes)
    })
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
        // sub dir + a.txt + sub/b.txt = 3
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
        // In contents_first mode, file appears before its parent directory
        let file_idx = entries.iter().position(|e| e.path().is_file()).unwrap();
        let dir_idx = entries.iter().position(|e| e.path() == sub).unwrap();
        assert!(file_idx < dir_idx);
    }
}
