use crate::cli::Commands;
use crate::core::remote::{expand_remote_star, parse_remote_path};
use anyhow::{bail, Context, Result};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

#[derive(Eq, PartialEq, Hash)]
enum SourceIdentity {
    Local(PathBuf),
    Remote(String, String),
}

fn source_identity(source: &Path) -> SourceIdentity {
    if let Some(remote) = parse_remote_path(&source.to_string_lossy()) {
        let path = remote
            .path
            .split('/')
            .filter(|component| !component.is_empty() && *component != ".")
            .collect::<Vec<_>>()
            .join("/");
        let path = if remote.path.starts_with('/') {
            format!("/{path}")
        } else {
            path
        };
        SourceIdentity::Remote(remote.ssh_target(), path)
    } else {
        // Normalize lexical spellings only; canonicalizing would conflate
        // distinct symbolic links and change which source a move removes.
        SourceIdentity::Local(
            source
                .components()
                .filter(|component| !matches!(component, Component::CurDir))
                .collect(),
        )
    }
}

/// Resolve one source snapshot for planning, fan-out and successful move cleanup.
pub(crate) async fn expand_copy_move_sources(command: &mut Commands) -> Result<()> {
    let args = match command {
        Commands::Copy { args, .. } | Commands::Move { args, .. } => args,
        _ => return Ok(()),
    };
    let count = if args.to.is_empty() {
        args.paths.len().saturating_sub(1)
    } else {
        args.paths.len()
    };
    let mut expanded = Vec::new();
    let mut seen = HashSet::new();
    for source in &args.paths[..count] {
        let remote = parse_remote_path(&source.to_string_lossy());
        let has_star = remote.as_ref().map_or_else(
            || source.as_os_str().as_encoded_bytes().contains(&b'*'),
            |remote| remote.path.contains('*'),
        );
        let matches = if !has_star {
            vec![source.clone()]
        } else if let Some(remote) = remote {
            expand_remote_star(remote)
                .await
                .with_context(|| format!("Source pattern '{}'", source.display()))?
                .into_iter()
                .map(|remote| PathBuf::from(remote.display()))
                .collect()
        } else {
            crate::core::star::expand_local(source)
                .with_context(|| format!("Source pattern '{}'", source.display()))?
        };
        if matches.is_empty() {
            bail!("No matches for source pattern '{}'", source.display());
        }
        for matched in matches {
            if seen.insert(source_identity(&matched)) {
                expanded.push(matched);
            }
        }
    }
    expanded.extend_from_slice(&args.paths[count..]);
    args.paths = expanded;
    Ok(())
}
