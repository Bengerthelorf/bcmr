mod exec;
mod file_copy;
mod overwrite;
mod pipeline_batch;
mod plan;
mod symlinks;

#[allow(unused_imports)]
pub use exec::{copy_path, execute_plan};
pub use overwrite::{check_overwrites, get_total_size, FileToOverwrite};
pub use pipeline_batch::{pipeline_copy, PipelineCallbacks};
#[cfg(any(windows, test))]
pub(crate) use plan::windows_symlink_kind_from_attributes;
pub use plan::{dry_run_plan, plan_copy, plan_copy_exact};
pub(crate) use plan::{is_dst_self_or_under_src, SymlinkKind};
pub(crate) use symlinks::create_symlink_replacing;

pub(crate) use exec::{preserve_staging_attributes, verify_copy};
#[cfg(test)]
pub(crate) use file_copy::create_staging;
pub(crate) use file_copy::{
    destination_parent, validate_direct_destination, AtomicStaging, CommitPolicy,
};
