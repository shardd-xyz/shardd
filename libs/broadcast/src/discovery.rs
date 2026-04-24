use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use libp2p::Multiaddr;
use libp2p::identity::{self, Keypair};
use sha2::{Digest, Sha256};

pub fn parse_bootstrap_peers(peers: &[String]) -> Result<Vec<Multiaddr>> {
    peers
        .iter()
        .map(|peer| {
            peer.parse()
                .with_context(|| format!("invalid bootstrap peer multiaddr: {peer}"))
        })
        .collect()
}

pub fn load_psk_file(path: impl AsRef<Path>) -> Result<[u8; 32]> {
    let path = path.as_ref();
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    if bytes.len() < 32 {
        bail!("PSK file {} must contain at least 32 bytes", path.display());
    }

    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes[..32]);
    Ok(key)
}

pub fn derive_psk_from_cluster_key(cluster_key: &str) -> Result<[u8; 32]> {
    let trimmed = cluster_key.trim();
    if trimmed.is_empty() {
        bail!("cluster key must not be empty");
    }
    let digest = Sha256::digest(trimmed.as_bytes());
    let mut key = [0u8; 32];
    key.copy_from_slice(&digest[..32]);
    Ok(key)
}

/// Deterministically derive a libp2p ed25519 keypair from the mesh PSK and a
/// stable per-node identifier.
///
/// The PSK (32 bytes derived from `cluster_key`) is the secret half of the
/// seed: only nodes inside the private mesh have it. Mixing it with a public
/// identifier (`node_id` UUID for full nodes, `public_edge_id` for gateways)
/// makes the resulting private key unguessable from outside the mesh while
/// still being identical across restarts of the same node — which is what
/// keeps peer caches, Kademlia routing tables, and peer-id-pinned dials
/// valid after a redeploy.
///
/// Without the PSK as the secret half, a `node_id` UUID broadcast in
/// Identify metadata would be enough to reconstruct any node's private key
/// and impersonate it.
pub fn derive_keypair_from_seed(psk: &[u8; 32], identifier: &str) -> Result<Keypair> {
    let trimmed = identifier.trim();
    if trimmed.is_empty() {
        bail!("keypair identifier must not be empty");
    }
    let mut hasher = Sha256::new();
    hasher.update(b"shardd/libp2p/identity/v1");
    hasher.update(psk);
    hasher.update(b":");
    hasher.update(trimmed.as_bytes());
    let digest = hasher.finalize();
    let mut secret = [0u8; 32];
    secret.copy_from_slice(&digest[..32]);
    let keypair = identity::ed25519::Keypair::from(
        identity::ed25519::SecretKey::try_from_bytes(&mut secret)
            .map_err(|e| anyhow::anyhow!("ed25519 secret from bytes: {e}"))?,
    );
    Ok(Keypair::from(keypair))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cluster_key_derivation_is_stable() {
        let a = derive_psk_from_cluster_key("cluster-key").unwrap();
        let b = derive_psk_from_cluster_key("cluster-key").unwrap();
        let c = derive_psk_from_cluster_key("other-key").unwrap();
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn keypair_seed_is_stable_and_secret_dependent() {
        let psk_a = derive_psk_from_cluster_key("cluster-key").unwrap();
        let psk_b = derive_psk_from_cluster_key("other-key").unwrap();

        // Same psk + same identifier -> identical keypair
        let kp1 = derive_keypair_from_seed(&psk_a, "node-1").unwrap();
        let kp2 = derive_keypair_from_seed(&psk_a, "node-1").unwrap();
        assert_eq!(kp1.public().to_peer_id(), kp2.public().to_peer_id());

        // Same psk, different identifier -> different keypair
        let kp_other_id = derive_keypair_from_seed(&psk_a, "node-2").unwrap();
        assert_ne!(kp1.public().to_peer_id(), kp_other_id.public().to_peer_id());

        // Different psk, same identifier -> different keypair
        // (this is what defends against pre-image attacks based on the
        // public node_id alone)
        let kp_other_psk = derive_keypair_from_seed(&psk_b, "node-1").unwrap();
        assert_ne!(
            kp1.public().to_peer_id(),
            kp_other_psk.public().to_peer_id()
        );
    }

    #[test]
    fn keypair_seed_rejects_empty_identifier() {
        let psk = derive_psk_from_cluster_key("cluster-key").unwrap();
        assert!(derive_keypair_from_seed(&psk, "").is_err());
        assert!(derive_keypair_from_seed(&psk, "   ").is_err());
    }
}
