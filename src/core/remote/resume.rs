use super::RemoteTransferOptions;
use crate::core::error::BcmrError;

pub struct ResumeDecision {
    pub skip_bytes: u64,
    pub use_append_mode: bool,
    pub skip_entirely: bool,
}

pub async fn check_resume_state(
    opts: &RemoteTransferOptions,
    existing_size: Option<u64>,
    source_size: u64,
    existing_full_hash: impl AsyncFnOnce() -> Result<String, BcmrError>,
    source_full_hash: impl AsyncFnOnce() -> Result<String, BcmrError>,
    source_partial_hash: impl AsyncFnOnce(u64) -> Result<String, BcmrError>,
) -> Result<ResumeDecision, BcmrError> {
    if !(opts.resume || opts.append || opts.strict) {
        return Ok(ResumeDecision {
            skip_bytes: 0,
            use_append_mode: false,
            skip_entirely: false,
        });
    }

    let existing_size = match existing_size {
        Some(s) => s,
        None => {
            return Ok(ResumeDecision {
                skip_bytes: 0,
                use_append_mode: false,
                skip_entirely: false,
            })
        }
    };

    if existing_size == source_size {
        let ex_hash = existing_full_hash().await?;
        let src_hash = source_full_hash().await?;
        if ex_hash == src_hash {
            return Ok(ResumeDecision {
                skip_bytes: 0,
                use_append_mode: false,
                skip_entirely: true,
            });
        }
        return Ok(ResumeDecision {
            skip_bytes: 0,
            use_append_mode: false,
            skip_entirely: false,
        });
    } else if existing_size < source_size {
        let ex_hash = existing_full_hash().await?;
        let partial = source_partial_hash(existing_size).await?;
        if ex_hash == partial {
            return Ok(ResumeDecision {
                skip_bytes: existing_size,
                use_append_mode: true,
                skip_entirely: false,
            });
        }
        return Ok(ResumeDecision {
            skip_bytes: 0,
            use_append_mode: false,
            skip_entirely: false,
        });
    }

    Ok(ResumeDecision {
        skip_bytes: 0,
        use_append_mode: false,
        skip_entirely: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_check_resume_state_same_size_requires_hash_match() {
        let opts = RemoteTransferOptions {
            resume: true,
            ..Default::default()
        };

        let decision = check_resume_state(
            &opts,
            Some(1024),
            1024,
            async || Ok("existing".to_string()),
            async || Ok("source".to_string()),
            async |_| Ok("unused".to_string()),
        )
        .await
        .expect("decision should compute");

        assert!(!decision.skip_entirely);
        assert_eq!(decision.skip_bytes, 0);
        assert!(!decision.use_append_mode);
    }

    #[tokio::test]
    async fn test_check_resume_state_shorter_prefix_requires_hash_match() {
        let opts = RemoteTransferOptions {
            append: true,
            ..Default::default()
        };

        let decision = check_resume_state(
            &opts,
            Some(512),
            1024,
            async || Ok("corrupt-prefix".to_string()),
            async || Ok("unused".to_string()),
            async |_| Ok("source-prefix".to_string()),
        )
        .await
        .expect("decision should compute");

        assert!(!decision.skip_entirely);
        assert_eq!(decision.skip_bytes, 0);
        assert!(!decision.use_append_mode);
    }

    #[tokio::test]
    async fn test_check_resume_state_matching_prefix_allows_append() {
        let opts = RemoteTransferOptions {
            resume: true,
            ..Default::default()
        };

        let decision = check_resume_state(
            &opts,
            Some(512),
            1024,
            async || Ok("prefix-hash".to_string()),
            async || Ok("full-hash".to_string()),
            async |_| Ok("prefix-hash".to_string()),
        )
        .await
        .expect("decision should compute");

        assert!(!decision.skip_entirely);
        assert_eq!(decision.skip_bytes, 512);
        assert!(decision.use_append_mode);
    }
}
