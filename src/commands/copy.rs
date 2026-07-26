mod exec;
mod file_copy;
mod overwrite;
mod pipeline_batch;
mod plan;
mod symlinks;

pub use exec::{copy_path, execute_plan};
pub use overwrite::{check_overwrites, get_total_size, FileToOverwrite};
pub use pipeline_batch::{pipeline_copy, PipelineCallbacks};
pub use plan::{dry_run_plan, plan_copy};

pub(crate) use exec::{preserve_attributes, verify_copy};
#[cfg(test)]
pub(crate) use file_copy::create_staging;
pub(crate) use file_copy::AtomicStaging;
