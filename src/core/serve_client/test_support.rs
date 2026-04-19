use crate::core::error::BcmrError;
use crate::core::framing;
use crate::core::protocol::CompressionAlgo;
use crate::core::transport::ssh as ssh_transport;

use super::{ServeClient, CLIENT_CAPS};

impl ServeClient {
    #[allow(dead_code)]
    pub async fn connect_local_with_caps(caps: u8) -> Result<Self, BcmrError> {
        let mut client = Self::spawn_local_serve().await?;
        client.handshake(caps).await?;
        Ok(client)
    }

    #[allow(dead_code)]
    pub async fn connect_local() -> Result<Self, BcmrError> {
        let mut client = Self::spawn_local_serve().await?;
        client.handshake(CLIENT_CAPS).await?;
        Ok(client)
    }

    #[allow(dead_code)]
    pub async fn connect_direct_local_with_caps(caps: u8) -> Result<Self, BcmrError> {
        let bcmr_path = Self::locate_bcmr_binary()?;
        let spawn = ssh_transport::spawn_local(&bcmr_path).await?;
        Self::promote_to_direct_tcp(spawn, caps, None).await
    }

    #[allow(dead_code)]
    pub async fn connect_direct_local() -> Result<Self, BcmrError> {
        Self::connect_direct_local_with_caps(CLIENT_CAPS).await
    }

    #[allow(dead_code)]
    async fn spawn_local_serve() -> Result<Self, BcmrError> {
        let bcmr_path = Self::locate_bcmr_binary()?;
        let spawn = ssh_transport::spawn_local(&bcmr_path).await?;
        Ok(Self::from_ssh_spawn(spawn))
    }

    #[allow(dead_code)]
    fn locate_bcmr_binary() -> Result<std::path::PathBuf, BcmrError> {
        let exe = std::env::current_exe()?;
        let bin_dir = exe
            .parent()
            .ok_or_else(|| BcmrError::InvalidInput("cannot find binary directory".into()))?;

        let bin_name = if cfg!(windows) { "bcmr.exe" } else { "bcmr" };

        let candidates = [
            bin_dir.join(bin_name),
            bin_dir
                .parent()
                .map(|p| p.join(bin_name))
                .unwrap_or_default(),
        ];
        candidates
            .iter()
            .find(|p| p.exists())
            .cloned()
            .ok_or_else(|| {
                BcmrError::InvalidInput(format!(
                    "bcmr binary not found at {} or {}",
                    candidates[0].display(),
                    candidates[1].display()
                ))
            })
    }

    #[allow(dead_code)]
    pub fn negotiated_algo(&self) -> CompressionAlgo {
        self.algo
    }

    #[allow(dead_code)]
    pub fn is_aead_negotiated(&self) -> bool {
        matches!(self.tx.as_ref(), Some(framing::SendHalf::Aead { .. }))
    }
}
