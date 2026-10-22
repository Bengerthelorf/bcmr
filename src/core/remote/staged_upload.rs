//! Fresh uploads publish only after content, verification and metadata succeed.
//!
//! Shell peers cannot provide AtomicFile's handle-bound transaction or durable
//! directory fsync. Existing security attributes are copied with checked GNU
//! or native macOS tools before exact-target publication.
use super::ssh_cmd::{shell_escape, ssh_command, ssh_error_message};
use super::RemotePath;
use crate::core::error::BcmrError;

const ID_FUNCTION: &str = r#"
entry_id() {
    LC_ALL=C stat -c '%d %i' "$1" 2>/dev/null || LC_ALL=C stat -f '%d %i' "$1"
}
"#;

pub struct StagedUpload {
    remote: RemotePath,
    parent: String,
    name: String,
    directory: String,
    parent_id: String,
    directory_id: String,
    security: Option<(u32, u32)>,
    mode: u32,
    allow_overwrite: bool,
    prepared_identity: Option<String>,
    prepared_symlink: bool,
    committed: bool,
}

impl StagedUpload {
    pub async fn create(target: &RemotePath, allow_overwrite: bool) -> Result<Self, BcmrError> {
        let (parent, name) = target.path.rsplit_once('/').unwrap_or((".", &target.path));
        let parent = if parent.is_empty() { "/" } else { parent };
        if name.is_empty() || matches!(name, "." | "..") {
            return Err(BcmrError::InvalidInput(
                "upload requires a file basename".into(),
            ));
        }
        let script = format!(
            r#"{ID_FUNCTION}
parent=$1
name=$2
allow=$3
cd "$parent" || exit 1
parent=$(pwd -P) || exit 1
parent_id=$(entry_id .) || exit 1
if [ ! -L "./$name" ] && [ -e "./$name" ] && [ ! -f "./$name" ]; then
    printf '%s\n' 'upload destination is not a regular file' >&2
    exit 1
fi
if [ -e "./$name" ] || [ -L "./$name" ]; then
    if [ "$allow" != 1 ]; then
        printf '%s\n' 'upload destination already exists' >&2
        exit 1
    fi
    if [ ! -L "./$name" ]; then
        security=$(LC_ALL=C stat -c '%u %g %a' "./$name" 2>/dev/null || LC_ALL=C stat -f '%u %g %Lp' "./$name") || exit 1
    fi
fi
if [ -z "${{security-}}" ]; then
    mask=$(umask)
    mode=$(printf '%o' "$((0666 & ~0$mask))") || exit 1
    security="new $mode"
fi
umask 077
directory=$(mktemp -d './.bcmr.upload.XXXXXXXXXX') || exit 1
directory=${{directory#./}}
if [ "$(uname -s)" = Darwin ]; then
    /bin/chmod -N "./$directory" || {{ rmdir "./$directory"; exit 1; }}
fi
directory_id=$(entry_id "./$directory") || {{ rmdir "./$directory"; exit 1; }}
printf '%s\000%s\000%s\000%s\000%s\000' "$parent" "$directory" "$parent_id" "$directory_id" "$security"
"#
        );
        let output = ssh_command(&target.ssh_target())
            .arg(command(
                &script,
                "bcmr-upload-create",
                &[parent, name, if allow_overwrite { "1" } else { "0" }],
            ))
            .output()
            .await?;
        checked_status(&output, "Failed to create private upload staging")?;
        let fields: Vec<_> = output.stdout.split(|byte| *byte == 0).collect();
        if fields.len() != 6 || !fields[5].is_empty() {
            return Err(BcmrError::InvalidInput(
                "invalid private upload staging response".into(),
            ));
        }
        let field = |index: usize| -> Result<String, BcmrError> {
            String::from_utf8(fields[index].to_vec())
                .map_err(|_| BcmrError::InvalidInput("non-UTF-8 upload staging response".into()))
        };
        let parent = field(0)?;
        let directory = field(1)?;
        if !directory.starts_with(".bcmr.upload.") || directory.contains('/') {
            return Err(BcmrError::InvalidInput(
                "invalid upload transaction name".into(),
            ));
        }
        let parent_id = field(2)?;
        let directory_id = field(3)?;
        validate_identity(&parent_id)?;
        validate_identity(&directory_id)?;
        let security_text = field(4)?;
        let parts: Vec<_> = security_text.split_whitespace().collect();
        let (security, mode) = match parts.as_slice() {
            ["new", mode] => (None, parse_mode(mode)?),
            [uid, gid, mode] => {
                let uid = uid
                    .parse()
                    .map_err(|_| BcmrError::InvalidInput("invalid upload owner".into()))?;
                let gid = gid
                    .parse()
                    .map_err(|_| BcmrError::InvalidInput("invalid upload group".into()))?;
                (Some((uid, gid)), parse_mode(mode)? & 0o777)
            }
            _ => {
                return Err(BcmrError::InvalidInput(
                    "invalid upload security metadata".into(),
                ))
            }
        };
        let remote = RemotePath {
            path: format!("{}/{directory}/{name}", parent.trim_end_matches('/')),
            ..target.clone()
        };
        Ok(Self {
            remote,
            parent,
            name: name.to_string(),
            directory,
            parent_id,
            directory_id,
            security,
            mode,
            allow_overwrite,
            prepared_identity: None,
            prepared_symlink: false,
            committed: false,
        })
    }

    pub fn remote(&self) -> &RemotePath {
        &self.remote
    }

    fn guard(&self) -> String {
        format!(
            r#"{ID_FUNCTION}
cd '{}' || exit 1
[ "$(entry_id .)" = '{}' ] || exit 1
directory='{}'
name='{}'
[ ! -L "./$directory" ] && [ -d "./$directory" ] || exit 1
[ "$(entry_id "./$directory")" = '{}' ] || exit 1
payload="./$directory/$name"
"#,
            shell_escape(&self.parent),
            shell_escape(&self.parent_id),
            shell_escape(&self.directory),
            shell_escape(&self.name),
            shell_escape(&self.directory_id)
        )
    }

    /// Restore portable destination security before optional source `-p` attrs.
    pub async fn prepare(&mut self) -> Result<(), BcmrError> {
        let security_copy = if self.security.is_some() {
            r#"
[ ! -L "./$name" ] && [ -f "./$name" ] || exit 1
if [ "$(uname -s)" = Darwin ]; then
    # Native cp -p preserves macOS ACLs and extended attributes. A private
    # sidecar is needed because BSD cp has no attributes-only operation.
    metadata=$(mktemp "./$directory/.bcmr.metadata.XXXXXXXXXX") || exit 1
    trap 'if [ -n "$metadata" ]; then rm -f "$metadata"; fi' 0 HUP INT TERM
    /bin/cp -p "./$name" "$metadata" || exit 1
    chmod u+w "$metadata" || exit 1
    cat "$payload" > "$metadata" || exit 1
    mv -f "$metadata" "$payload" || exit 1
    metadata=
    trap - 0 HUP INT TERM
elif cp --version >/dev/null 2>&1; then
    # Explicit xattr preservation (rather than --preserve=all) makes a
    # failure to retain those attributes affect cp's exit status.
    cp --attributes-only --preserve=mode,ownership,xattr -- "./$name" "$payload" || exit 1
else
    printf '%s\n' 'safe existing-file ACL/xattr preservation requires GNU cp or native macOS cp' >&2
    exit 1
fi
"#
        } else {
            ""
        };
        let ownership = self
            .security
            .map(|(uid, gid)| format!("chown {uid}:{gid} \"$payload\" || exit 1\n"))
            .unwrap_or_default();
        let script = format!(
            "{}[ ! -L \"$payload\" ] && [ -f \"$payload\" ] || exit 1\n{}{}chmod {:o} \"$payload\" || exit 1\nentry_id \"$payload\"\n",
            self.guard(), security_copy, ownership, self.mode
        );
        let output = ssh_command(&self.remote.ssh_target())
            .arg(command(&script, "bcmr-upload-prepare", &[]))
            .output()
            .await?;
        checked_status(&output, "Failed to prepare upload security metadata")?;
        let identity = String::from_utf8(output.stdout)
            .map_err(|_| BcmrError::InvalidInput("invalid staged upload identity".into()))?;
        let identity = identity.trim().to_string();
        validate_identity(&identity)?;
        self.prepared_identity = Some(identity);
        self.prepared_symlink = false;
        Ok(())
    }

    /// Prepare an existing staged link without ever opening its target.
    pub async fn prepare_symlink(&mut self) -> Result<(), BcmrError> {
        let script = format!(
            "{}[ -L \"$payload\" ] || exit 1\nentry_id \"$payload\"\n",
            self.guard()
        );
        let output = ssh_command(&self.remote.ssh_target())
            .arg(command(&script, "bcmr-upload-prepare-link", &[]))
            .output()
            .await?;
        checked_status(&output, "Failed to prepare staged link")?;
        let identity = String::from_utf8(output.stdout)
            .map_err(|_| BcmrError::InvalidInput("invalid staged link identity".into()))?;
        let identity = identity.trim().to_string();
        validate_identity(&identity)?;
        self.prepared_identity = Some(identity);
        self.prepared_symlink = true;
        Ok(())
    }

    pub async fn commit(&mut self) -> Result<(), BcmrError> {
        let identity = self.prepared_identity.as_ref().ok_or_else(|| {
            BcmrError::InvalidInput("upload was not prepared before publication".into())
        })?;
        let publish = if self.allow_overwrite {
            // The payload basename is the FINAL basename. The destination is
            // always the pinned cwd, never a user path reinterpreted as a dir.
            if self.prepared_symlink {
                // Darwin's directory-form mv compares link referents in
                // unix2003 mode. Legacy mode retains the exact-target rename
                // while allowing distinct links to the same live referent.
                "if [ \"$(uname -s)\" = Darwin ]; then\n    COMMAND_MODE=legacy /bin/mv -f \"$payload\" . || exit 1\nelse\n    mv -f \"$payload\" . || exit 1\nfi\n"
            } else {
                "mv -f \"$payload\" . || exit 1\n"
            }
        } else if self.prepared_symlink {
            // BSD link(2) follows source links. ln -P explicitly links the
            // staged link object; the pinned cwd fixes the destination once.
            "ln -P \"$payload\" . || exit 1\nrm -f \"$payload\" || exit 1\n"
        } else {
            // Unlike ln, the link utility invokes link(2) with an exact target.
            "link \"$payload\" \"./$name\" || exit 1\nrm -f \"$payload\" || exit 1\n"
        };
        let kind_guard = if self.prepared_symlink {
            "[ -L \"$payload\" ]"
        } else {
            "[ ! -L \"$payload\" ] && [ -f \"$payload\" ]"
        };
        let script = format!(
            "{}{} || exit 1\n[ \"$(entry_id \"$payload\")\" = '{}' ] || exit 1\n{}rmdir \"./$directory\" || exit 1\n",
            self.guard(), kind_guard, shell_escape(identity), publish
        );
        let output = ssh_command(&self.remote.ssh_target())
            .arg(command(&script, "bcmr-upload-commit", &[]))
            .output()
            .await?;
        checked_status(&output, "Failed to publish staged upload")?;
        self.committed = true;
        Ok(())
    }

    /// Remove only this guarded payload and its empty private directory.
    pub async fn cleanup(&self) {
        if self.committed {
            return;
        }
        let script = format!(
            "{}rm -f \"$payload\" && rmdir \"./$directory\"\n",
            self.guard()
        );
        let _ = ssh_command(&self.remote.ssh_target())
            .arg(command(&script, "bcmr-upload-cleanup", &[]))
            .output()
            .await;
    }
}

fn command(script: &str, label: &str, args: &[&str]) -> String {
    let mut result = format!("sh -c '{}' {}", shell_escape(script), label);
    for argument in args {
        result.push_str(&format!(" '{}'", shell_escape(argument)));
    }
    result
}

fn checked_status(output: &std::process::Output, context: &str) -> Result<(), BcmrError> {
    if output.status.success() {
        return Ok(());
    }
    Err(BcmrError::InvalidInput(ssh_error_message(
        &String::from_utf8_lossy(&output.stderr),
        context,
    )))
}

fn validate_identity(identity: &str) -> Result<(), BcmrError> {
    let parts: Vec<_> = identity.split_whitespace().collect();
    if parts.len() == 2
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Ok(());
    }
    Err(BcmrError::InvalidInput(
        "invalid remote entry identity".into(),
    ))
}

fn parse_mode(text: &str) -> Result<u32, BcmrError> {
    u32::from_str_radix(text, 8)
        .ok()
        .filter(|mode| *mode <= 0o7777)
        .ok_or_else(|| BcmrError::InvalidInput("invalid upload mode".into()))
}
