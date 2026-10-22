//! One selected plan for mixed sources, SFTP-only peers and remote relays.
use super::endpoint::{Endpoint, Kind, Reader, Writer};
use crate::cli::{Commands, CompressionMode, DirectMode};
use crate::core::error::BcmrError;
use crate::core::remote::{self, backend, move_delivery, sftp_ops, sftp_stage::SftpStage};
use crate::ui::runner::{ProgressHandle, ProgressRunner};
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

mod plan;
pub(super) use plan::required;
use plan::Entry;

/// Ordinary SSH transfers use the same host-qualified file identities as relay
/// moves. Equal paths or inode numbers alone do not identify files across hosts.
pub(super) async fn validate_regular_file_pair(source: &Endpoint, target: &Endpoint) -> Result<()> {
    plan::validate_regular_file_pair(source, target).await
}

/// Ordinary link-preserving SSH paths share the same mutation-free guards as
/// mixed/relay plans, including destination self-loops and move referents.
pub(super) async fn validate_link_plan(
    source: Endpoint,
    target: Endpoint,
    link: String,
    link_kind: crate::commands::copy::SymlinkKind,
    moving: bool,
    overwrite: bool,
) -> Result<()> {
    let entry = Entry {
        source,
        target,
        link: Some(link),
        link_kind,
        kind: Kind::Link,
        size: 0,
        metadata: None,
    };
    plan::validate_link_destination(&entry).await?;
    if moving {
        plan::validate_move_leaf(&entry).await?;
    } else if overwrite && entry.target.kind().await?.is_some() {
        plan::validate_copy_leaf(&entry).await?;
    }
    Ok(())
}

fn interrupted() -> anyhow::Error {
    BcmrError::Io(std::io::Error::new(
        std::io::ErrorKind::Interrupted,
        "operation interrupted",
    ))
    .into()
}

fn check_cancel(cancel: &crate::ui::runner::ProgressCancellation) -> Result<()> {
    if cancel.is_cancelled() {
        crate::core::interrupt::request();
    }
    crate::core::interrupt::check()?;
    Ok(())
}

fn remember_delivery(
    entry: &Entry,
    args: &Commands,
    content: move_delivery::Content,
) -> Result<()> {
    if !matches!(args, Commands::Move { .. }) {
        return Ok(());
    }
    let target = match &entry.target {
        Endpoint::Local(path) => move_delivery::Target::Local(path.clone()),
        Endpoint::Ssh(remote) | Endpoint::Sftp(remote) => {
            move_delivery::Target::Remote(remote.clone())
        }
    };
    match &entry.source {
        Endpoint::Ssh(source) | Endpoint::Sftp(source) => {
            move_delivery::remember(source, target, content, args.is_preserve())
        }
        Endpoint::Local(source) => {
            move_delivery::remember_local(source, target, content, args.is_preserve())?
        }
    }
    Ok(())
}

async fn copy_link(entry: &Entry, args: &Commands) -> Result<()> {
    let target = entry
        .link
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("missing symbolic-link target"))?;
    if matches!(args, Commands::Move { .. }) {
        if let Endpoint::Sftp(source) = &entry.source {
            let expected = sftp_ops::captured_move_link(source)
                .ok_or_else(|| anyhow::anyhow!("missing captured SFTP move link"))?;
            if expected != target.as_bytes() {
                bail!("SFTP move source link changed after selection; source preserved");
            }
        }
    }
    entry.target.parent().ensure_dir(args.is_sync()).await?;
    match &entry.target {
        Endpoint::Local(path) => {
            crate::commands::copy::create_symlink_replacing(
                path,
                Path::new(target),
                entry.link_kind,
                args.is_force(),
                args.is_sync(),
                &args.get_test_mode(),
            )
            .await?
        }
        Endpoint::Ssh(remote) => {
            remote::create_remote_symlink(remote, target, args.is_force()).await?
        }
        Endpoint::Sftp(remote) => {
            let transaction = SftpStage::create(remote, args.is_force()).await?;
            let result = async {
                backend::session(&remote.ssh_target())
                    .await?
                    .lock()
                    .await
                    .symlink(target.as_bytes(), &transaction.payload.path)
                    .await?;
                transaction.capture_payload().await?;
                transaction.commit(None).await
            }
            .await;
            let cleanup = transaction.cleanup().await;
            match (result, cleanup) {
                (Err(error), Err(cleanup)) => {
                    return Err(anyhow::Error::from(error)
                        .context(format!("stage cleanup also failed: {cleanup}")))
                }
                (Err(error), _) | (_, Err(error)) => return Err(error.into()),
                _ => {}
            }
        }
    }
    remember_delivery(
        entry,
        args,
        move_delivery::Content::Link(target.as_bytes().to_vec()),
    )?;
    Ok(())
}

async fn copy_file(
    entry: &Entry,
    args: &Commands,
    progress: &ProgressHandle,
    cancel: &crate::ui::runner::ProgressCancellation,
) -> Result<()> {
    let options = super::transfer_options_from_cli(args);
    let captured_hash = if matches!(args, Commands::Move { .. }) {
        match &entry.source {
            Endpoint::Sftp(remote) => Some(
                sftp_ops::captured_move_hash(remote)
                    .ok_or_else(|| anyhow::anyhow!("missing captured SFTP move file content"))?,
            ),
            _ => None,
        }
    } else {
        None
    };
    let existing = entry.target.kind().await?;
    let mut prefix = 0u64;
    if existing == Some(Kind::File) && (options.resume || options.append || options.strict) {
        let existing_size = match &entry.target {
            Endpoint::Local(path) => path.metadata()?.len(),
            Endpoint::Ssh(remote) | Endpoint::Sftp(remote) => remote::remote_file_size(remote)
                .await?
                .ok_or_else(|| anyhow::anyhow!("destination vanished during resume"))?,
        };
        let decision = remote::check_resume_state(
            &options,
            Some(existing_size),
            entry.size,
            async || entry.target.hash(None).await,
            async || entry.source.hash(None).await,
            async |n| entry.source.hash(Some(n)).await,
        )
        .await?;
        if decision.skip_entirely {
            let hash = entry.target.hash(None).await?;
            if captured_hash
                .as_ref()
                .is_some_and(|expected| expected != &hash)
            {
                bail!("SFTP move source changed after selection; source preserved");
            }
            remember_delivery(entry, args, move_delivery::Content::File(hash))?;
            progress.inc_skipped(entry.size);
            return Ok(());
        }
        if decision.use_append_mode {
            prefix = decision.skip_bytes;
        }
    }
    let mut writer = Writer::create(
        &entry.target,
        options.force || options.resume || options.append || options.strict,
        options.sync,
    )
    .await?;
    let operation = async {
        let mut hasher = blake3::Hasher::new();
        if prefix > 0 {
            let mut reader = Reader::open(&entry.target, 0, Some(prefix)).await?;
            let mut copied = 0;
            while copied < prefix {
                let data = reader
                    .read((prefix - copied).min(64 * 1024) as usize)
                    .await?;
                if data.is_empty() {
                    bail!("resume destination shortened during prefix seeding");
                }
                copied += data.len() as u64;
                writer.write(&data).await?;
                hasher.update(&data);
            }
            reader.finish().await?;
            progress.inc_skipped(prefix);
        }
        let mut reader = Reader::open(&entry.source, prefix, None).await?;
        let mut copied = prefix;
        loop {
            let data = reader
                .read(crate::core::bandwidth::chunk_size().min(64 * 1024))
                .await?;
            if data.is_empty() {
                break;
            }
            copied = copied
                .checked_add(data.len() as u64)
                .ok_or_else(|| anyhow::anyhow!("transfer byte count overflow"))?;
            if copied > entry.size {
                bail!("source '{}' grew during transfer", entry.source.display());
            }
            crate::core::bandwidth::acquire(data.len()).await;
            writer.write(&data).await?;
            hasher.update(&data);
            progress.inc_current(data.len() as u64);
        }
        reader.finish().await?;
        if copied != entry.size {
            bail!(
                "source '{}' shortened during transfer",
                entry.source.display()
            );
        }
        let delivered_hash = hasher.finalize().to_hex().to_string();
        if captured_hash
            .as_ref()
            .is_some_and(|expected| expected != &delivered_hash)
        {
            bail!("SFTP move source content changed after selection; source preserved");
        }
        writer.close_stream().await?;
        writer.prepare().await?;
        let staging = writer.staging();
        if (options.verify || matches!(staging, Endpoint::Sftp(_)))
            && writer.staged_hash().await? != delivered_hash
        {
            return Err(BcmrError::VerificationError(entry.target.display().into()).into());
        }
        if let Some(metadata) = entry.metadata {
            if matches!(staging, Endpoint::Ssh(_)) {
                staging.set_metadata(metadata, false).await?;
            }
        }
        Ok::<_, anyhow::Error>(delivered_hash)
    };
    let result = tokio::select! {
        biased;
        _=crate::core::interrupt::cancelled()=>Err(interrupted()),
        _=cancel.cancelled()=>{crate::core::interrupt::request(); Err(interrupted())},
        result=operation=>result,
    };
    let delivered_hash = match result {
        Ok(hash) => hash,
        Err(error) => {
            let cleanup = writer.abort().await;
            entry.source.disconnect().await;
            entry.target.disconnect().await;
            return match cleanup {
                Ok(()) => Err(error),
                Err(cleanup) => Err(error.context(format!("stage cleanup also failed: {cleanup}"))),
            };
        }
    };
    if let Err(error) = check_cancel(cancel) {
        return match writer.abort().await {
            Ok(()) => Err(error),
            Err(cleanup) => Err(error.context(format!("stage cleanup also failed: {cleanup}"))),
        };
    }
    writer.commit(entry.metadata, options.sync).await?;
    remember_delivery(entry, args, move_delivery::Content::File(delivered_hash))?;
    check_cancel(cancel)?;
    Ok(())
}

pub(super) async fn copy(
    args: &Commands,
    sources: &[PathBuf],
    dest: &Path,
    excludes: &[regex::Regex],
) -> Result<()> {
    if args.get_reflink_mode().as_deref() == Some("force")
        || args.get_sparse_mode().as_deref() == Some("always")
    {
        bail!("composite/SFTP streams cannot satisfy forced reflink or sparse strategies; use auto/never or copy local sources separately");
    }
    if args
        .copy_move_args()
        .is_some_and(|args| !matches!(args.compress, CompressionMode::Auto | CompressionMode::None))
    {
        bail!("composite/SFTP transfers do not support bcmr zstd/lz4 framing; configure OpenSSH Compression with --ssh-config instead");
    }
    if args
        .copy_move_args()
        .is_some_and(|args| args.direct == DirectMode::Direct)
    {
        bail!(
            "composite/SFTP transfers use SSH; --direct=direct is unavailable for this combination"
        );
    }
    let entries = plan::build(args, sources, dest, excludes).await?;
    crate::core::interrupt::check()?;
    if args.is_dry_run() {
        for entry in &entries {
            if !crate::config::is_json_mode() {
                crate::ui::display::print_dry_run(
                    crate::ui::display::ActionType::Add,
                    &entry.source.display(),
                    Some(&entry.target.display()),
                );
            }
        }
        return super::finish_json_operation("Transferring (dry run)");
    }
    let total = entries
        .iter()
        .filter(|entry| entry.kind == Kind::File)
        .try_fold(0u64, |total, entry| {
            total
                .checked_add(entry.size)
                .ok_or_else(|| anyhow::anyhow!("transfer size overflow"))
        })?;
    let runner = ProgressRunner::new_without_signal_handler(
        total,
        super::progress_mode(args),
        args.is_quiet(),
        crate::config::is_json_mode(),
    )?;
    runner.set_operation_type("Transferring");
    runner.set_verify_mode(args.is_verify());
    let progress = runner.handle();
    let cancel = runner.cancellation_handle();
    let result = async {
        for entry in entries.iter().filter(|entry| entry.kind == Kind::Directory) {
            check_cancel(&cancel)?;
            entry.target.ensure_dir(args.is_sync()).await?;
        }
        for entry in entries.iter().filter(|entry| entry.kind != Kind::Directory) {
            check_cancel(&cancel)?;
            progress.set_current_file(&entry.source.display(), entry.size);
            if entry.kind == Kind::Link {
                copy_link(entry, args).await?;
            } else {
                copy_file(entry, args, &progress, &cancel).await?;
            }
            check_cancel(&cancel)?;
            progress.inc_items_processed();
        }
        for entry in entries
            .iter()
            .rev()
            .filter(|entry| entry.kind == Kind::Directory)
        {
            check_cancel(&cancel)?;
            if let Some(metadata) = entry.metadata {
                entry.target.set_metadata(metadata, args.is_sync()).await?;
            }
        }
        check_cancel(&cancel)?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    match result {
        Ok(()) => runner.finish_ok(),
        Err(error) => {
            runner.finish_with_error(&error.to_string());
            Err(error)
        }
    }
}
