use super::RemoteTransferOptions;
use crate::core::error::BcmrError;

#[derive(Debug)]
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
    let no_resume = ResumeDecision {
        skip_bytes: 0,
        use_append_mode: false,
        skip_entirely: false,
    };

    if !(opts.resume || opts.append || opts.strict) {
        return Ok(no_resume);
    }

    let existing_size = match existing_size {
        Some(s) => s,
        None => return Ok(no_resume),
    };

    let refuse = |reason: String| {
        Err(BcmrError::InvalidInput(format!(
            "--append refused: {reason} (use --resume to overwrite, or remove the flag)"
        )))
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
        if opts.append {
            return refuse(format!(
                "destination has the same size ({source_size}) but different content"
            ));
        }
        return Ok(no_resume);
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
        if opts.append {
            return refuse(format!(
                "destination's first {existing_size} bytes do not match source"
            ));
        }
        return Ok(no_resume);
    }

    if opts.append {
        return refuse(format!(
            "destination is larger ({existing_size}) than source ({source_size})"
        ));
    }
    Ok(no_resume)
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
            resume: true,
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

    #[tokio::test]
    async fn append_refuses_when_existing_larger_than_source() {
        let opts = RemoteTransferOptions {
            append: true,
            ..Default::default()
        };
        let err = check_resume_state(
            &opts,
            Some(2048),
            1024,
            async || Ok("unused".into()),
            async || Ok("unused".into()),
            async |_| Ok("unused".into()),
        )
        .await
        .expect_err("must refuse");
        assert!(err.to_string().contains("destination is larger"));
    }

    #[tokio::test]
    async fn append_refuses_when_prefix_diverges() {
        let opts = RemoteTransferOptions {
            append: true,
            ..Default::default()
        };
        let err = check_resume_state(
            &opts,
            Some(512),
            1024,
            async || Ok("ex".into()),
            async || Ok("unused".into()),
            async |_| Ok("src-prefix".into()),
        )
        .await
        .expect_err("must refuse");
        assert!(err.to_string().contains("first 512 bytes do not match"));
    }

    #[tokio::test]
    async fn append_refuses_when_same_size_but_different_content() {
        let opts = RemoteTransferOptions {
            append: true,
            ..Default::default()
        };
        let err = check_resume_state(
            &opts,
            Some(1024),
            1024,
            async || Ok("ex".into()),
            async || Ok("src".into()),
            async |_| Ok("unused".into()),
        )
        .await
        .expect_err("must refuse");
        assert!(err.to_string().contains("same size"));
    }

    #[tokio::test]
    async fn resume_still_overwrites_on_mismatch() {
        let opts = RemoteTransferOptions {
            resume: true,
            ..Default::default()
        };
        let decision = check_resume_state(
            &opts,
            Some(2048),
            1024,
            async || Ok("ex".into()),
            async || Ok("unused".into()),
            async |_| Ok("unused".into()),
        )
        .await
        .expect("resume must not refuse");
        assert!(!decision.skip_entirely);
        assert_eq!(decision.skip_bytes, 0);
        assert!(!decision.use_append_mode);
    }
}
