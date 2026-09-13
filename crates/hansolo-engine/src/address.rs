//! Payout address validation.
//!
//! The address is checked before anything starts: a typo here means a found
//! block pays nobody (node mode) or the pool refuses to authorise (Stratum).
//! Mainnet, testnet, signet and regtest encodings are all accepted; node mode
//! additionally checks the address against the chain the node reports.

use std::str::FromStr;

use bitcoin::address::NetworkUnchecked;
use bitcoin::{Address, Network, ScriptBuf};

/// A parsed payout address and the output script that pays it.
#[derive(Clone, Debug)]
pub struct PayoutAddress {
    unchecked: Address<NetworkUnchecked>,
    pub script_pubkey: ScriptBuf,
}

impl PayoutAddress {
    /// Whether the address encoding belongs to `network` (e.g. `tb1…` is valid
    /// for testnet and signet, `bcrt1…` only for regtest).
    pub fn is_valid_for(&self, network: Network) -> bool {
        self.unchecked.is_valid_for_network(network)
    }
}

/// Parses a Bitcoin address of any network.
pub fn parse_payout_address(address: &str) -> Result<PayoutAddress, String> {
    let address = address.trim();
    if address.is_empty() {
        return Err("a payout address is required".into());
    }
    let unchecked = Address::<NetworkUnchecked>::from_str(address)
        .map_err(|e| format!("invalid payout address {address:?}: {e}"))?;
    // Deriving the script does not depend on the network.
    let script_pubkey = unchecked.clone().assume_checked().script_pubkey();
    Ok(PayoutAddress {
        unchecked,
        script_pubkey,
    })
}

/// Maps `getblockchaininfo.chain` to a network.
pub(crate) fn network_from_chain(chain: &str) -> Option<Network> {
    Network::from_core_arg(chain).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_networks_and_derives_scripts() {
        let main = parse_payout_address("bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq").unwrap();
        assert!(main.is_valid_for(Network::Bitcoin));
        assert!(!main.is_valid_for(Network::Regtest));
        assert_eq!(
            hex::encode(main.script_pubkey.as_bytes()),
            "0014e8df018c7e326cc253faac7e46cdc51e68542c42"
        );

        let p2pkh = parse_payout_address("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa").unwrap();
        assert_eq!(
            hex::encode(p2pkh.script_pubkey.as_bytes()),
            "76a91462e907b15cbf27d5425399ebf6f0fb50ebb88f1888ac"
        );

        let test = parse_payout_address("tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx").unwrap();
        assert!(test.is_valid_for(Network::Testnet));
        assert!(test.is_valid_for(Network::Signet));
        assert!(!test.is_valid_for(Network::Bitcoin));

        let reg = parse_payout_address("bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080").unwrap();
        assert!(reg.is_valid_for(Network::Regtest));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_payout_address("").is_err());
        assert!(parse_payout_address("bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdx").is_err());
        assert!(parse_payout_address("hello").is_err());
    }

    #[test]
    fn chains() {
        assert_eq!(network_from_chain("main"), Some(Network::Bitcoin));
        assert_eq!(network_from_chain("test"), Some(Network::Testnet));
        assert_eq!(network_from_chain("signet"), Some(Network::Signet));
        assert_eq!(network_from_chain("regtest"), Some(Network::Regtest));
    }
}
