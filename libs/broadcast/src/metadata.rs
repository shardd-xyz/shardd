/// Identify protocol version string embedding the shardd protocol version.
pub const PROTOCOL_VERSION: &str = "/shardd/1.0.0";

const AGENT_PREFIX: &str = "shardd";

/// Metadata encoded into libp2p Identify `agent_version`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharddPeerMetadata {
    pub node_id: String,
    pub epoch: u32,
    pub advertise_addr: Option<String>,
}

/// Encode peer metadata into the Identify agent string.
///
/// Format:
/// - legacy: `shardd/{node_id}/{epoch}`
/// - current: `shardd/{node_id}/{epoch}/{advertise_addr}`
pub fn encode_agent_version(metadata: &SharddPeerMetadata) -> String {
    match metadata.advertise_addr.as_deref() {
        Some(addr) if !addr.is_empty() => {
            format!(
                "{AGENT_PREFIX}/{}/{}/{}",
                metadata.node_id, metadata.epoch, addr
            )
        }
        _ => format!("{AGENT_PREFIX}/{}/{}", metadata.node_id, metadata.epoch),
    }
}

/// Parse peer metadata from the Identify agent string.
pub fn parse_agent_version(agent_version: &str) -> Option<SharddPeerMetadata> {
    let mut parts = agent_version.splitn(4, '/');
    if parts.next()? != AGENT_PREFIX {
        return None;
    }

    let node_id = parts.next()?.to_string();
    let epoch = parts.next()?.parse().ok()?;
    let advertise_addr = parts
        .next()
        .map(str::trim)
        .filter(|addr| !addr.is_empty())
        .map(ToOwned::to_owned);

    Some(SharddPeerMetadata {
        node_id,
        epoch,
        advertise_addr,
    })
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_and_parse_with_advertise_addr() {
        let metadata = SharddPeerMetadata {
            node_id: "node-1".into(),
            epoch: 7,
            advertise_addr: Some("10.0.0.1:4001".into()),
        };

        let encoded = encode_agent_version(&metadata);
        assert_eq!(parse_agent_version(&encoded), Some(metadata));
    }

    #[test]
    fn parse_legacy_agent_version() {
        let parsed = parse_agent_version("shardd/abc-123/5").unwrap();
        assert_eq!(parsed.node_id, "abc-123");
        assert_eq!(parsed.epoch, 5);
        assert_eq!(parsed.advertise_addr, None);
    }

    #[test]
    fn parse_invalid_agent_version() {
        assert_eq!(parse_agent_version("other/agent"), None);
        assert_eq!(parse_agent_version("shardd/onlyone"), None);
        assert_eq!(parse_agent_version("shardd/node/not-a-number"), None);
    }
}
