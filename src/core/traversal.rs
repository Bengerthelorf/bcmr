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
    
    // implementing filter_entry for optimization
    walker.into_iter().filter_entry(move |e| {
        !is_excluded(e.path(), &excludes)
    })
}
