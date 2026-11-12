use rkn_core::ipc::Link;
use windows::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST, GAA_FLAG_SKIP_UNICAST, GetAdaptersAddresses,
    IF_TYPE_ETHERNET_CSMACD, IF_TYPE_IEEE80211, IP_ADAPTER_ADDRESSES_LH,
};
use windows::Win32::Networking::NetworkListManager::{
    IEnumNetworkConnections, IEnumNetworks, INetwork, INetworkListManager, NLM_CONNECTIVITY_IPV4_INTERNET,
    NLM_CONNECTIVITY_IPV6_INTERNET, NLM_ENUM_NETWORK_CONNECTED, NetworkListManager,
};
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Network {
    pub id: String,
    pub name: String,
    pub link: Link,
    pub internet: bool,
}

impl Network {
    pub fn unknown() -> Self {
        Self { id: "default".into(), name: String::new(), link: Link::Other, internet: false }
    }

    pub fn is_unknown(&self) -> bool {
        self.id == "default"
    }
}

/// The network the computer is on. With several connected at once (a VPN over Wi-Fi) the answer
/// must not depend on the order Windows lists them in: see `choose`.
pub fn current(prefer: Option<&str>) -> Option<Network> {
    let found = unsafe {
        let init = CoInitializeEx(None, COINIT_MULTITHREADED);
        let found = query().map(|all| choose(all, prefer));
        if init.is_ok() {
            CoUninitialize();
        }
        found
    };
    found.unwrap_or_else(|e| {
        tracing::debug!("network list: {e}");
        None
    })
}

/// Networks with internet come first. Among them a physical link (Wi-Fi, cable) wins over a
/// tunnel: the provider behind the physical link is what the strategy is learned for. `prefer` is the
/// network in use now, kept while it is still as good as the others.
fn choose(all: Vec<Network>, prefer: Option<&str>) -> Option<Network> {
    let rank = |n: &Network| (n.internet, n.link != Link::Other, prefer == Some(n.id.as_str()));
    let mut best: Option<Network> = None;
    for network in all {
        // Strictly better only: of equals the first one listed stays.
        if best.as_ref().is_none_or(|b| rank(&network) > rank(b)) {
            best = Some(network);
        }
    }
    best
}

unsafe fn query() -> windows::core::Result<Vec<Network>> {
    unsafe {
        let manager: INetworkListManager = CoCreateInstance(&NetworkListManager, None, CLSCTX_ALL)?;
        let networks: IEnumNetworks = manager.GetNetworks(NLM_ENUM_NETWORK_CONNECTED)?;
        let internet = NLM_CONNECTIVITY_IPV4_INTERNET.0 | NLM_CONNECTIVITY_IPV6_INTERNET.0;
        let mut all: Vec<Network> = Vec::new();
        loop {
            let mut item = [None];
            let mut fetched = 0u32;
            networks.Next(&mut item, Some(&mut fetched))?;
            let Some(network) = item[0].take() else { break };
            if fetched == 0 {
                break;
            }
            let id: String = format!("{:?}", network.GetNetworkId()?)
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect::<String>()
                .to_ascii_lowercase();
            let online = network.GetConnectivity()?.0 & internet != 0;
            all.push(Network { id, name: network.GetName()?.to_string(), link: link_of(&network), internet: online });
        }
        Ok(all)
    }
}

unsafe fn link_of(network: &INetwork) -> Link {
    unsafe {
        let Ok(connections): windows::core::Result<IEnumNetworkConnections> = network.GetNetworkConnections() else {
            return Link::Other;
        };
        let types = adapter_types();
        loop {
            let mut item = [None];
            let mut fetched = 0u32;
            if connections.Next(&mut item, Some(&mut fetched)).is_err() || fetched == 0 {
                break;
            }
            let Some(connection) = item[0].take() else { break };
            let Ok(adapter) = connection.GetAdapterId() else { continue };
            let key = guid_key(&format!("{adapter:?}"));
            match types.iter().find(|(name, _)| *name == key).map(|(_, t)| *t) {
                Some(IF_TYPE_IEEE80211) => return Link::Wifi,
                Some(IF_TYPE_ETHERNET_CSMACD) => return Link::Ethernet,
                _ => {}
            }
        }
        Link::Other
    }
}

fn guid_key(text: &str) -> String {
    text.chars().filter(char::is_ascii_hexdigit).collect::<String>().to_ascii_lowercase()
}

unsafe fn adapter_types() -> Vec<(String, u32)> {
    unsafe {
        let flags = GAA_FLAG_SKIP_UNICAST | GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
        let mut size = 16 * 1024u32;
        let mut buf: Vec<u64> = Vec::new();
        for _ in 0..3 {
            buf = vec![0u64; size as usize / 8 + 1];
            let head = buf.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
            match GetAdaptersAddresses(0, flags, None, Some(head), &mut size) {
                0 => {
                    let mut out = Vec::new();
                    let mut node = head;
                    while !node.is_null() {
                        let adapter = &*node;
                        let name: Result<String, _> = adapter.AdapterName.to_string();
                        if let Ok(name) = name {
                            out.push((guid_key(&name), adapter.IfType));
                        }
                        node = adapter.Next;
                    }
                    return out;
                }
                111 => continue,
                _ => break,
            }
        }
        drop(buf);
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(id: &str, link: Link, internet: bool) -> Network {
        Network { id: id.into(), name: id.into(), link, internet }
    }

    fn chosen(all: Vec<Network>, prefer: Option<&str>) -> Option<String> {
        choose(all, prefer).map(|n| n.id)
    }

    #[test]
    fn a_physical_link_wins_over_a_tunnel_in_any_order() {
        let (vpn, wifi) = (net("vpn", Link::Other, true), net("wifi", Link::Wifi, true));
        assert_eq!(chosen(vec![vpn.clone(), wifi.clone()], None).as_deref(), Some("wifi"));
        assert_eq!(chosen(vec![wifi.clone(), vpn.clone()], None).as_deref(), Some("wifi"));
        assert_eq!(chosen(vec![vpn, wifi], Some("vpn")).as_deref(), Some("wifi"));
    }

    #[test]
    fn of_equal_networks_the_current_one_stays() {
        let all = || vec![net("cable", Link::Ethernet, true), net("wifi", Link::Wifi, true)];
        assert_eq!(chosen(all(), None).as_deref(), Some("cable"));
        assert_eq!(chosen(all(), Some("wifi")).as_deref(), Some("wifi"));
        assert_eq!(chosen(all(), Some("gone")).as_deref(), Some("cable"));
    }

    #[test]
    fn internet_matters_most() {
        let all = vec![net("wifi", Link::Wifi, false), net("modem", Link::Other, true)];
        assert_eq!(chosen(all, Some("wifi")).as_deref(), Some("modem"));
        assert_eq!(chosen(vec![net("wifi", Link::Wifi, false)], None).as_deref(), Some("wifi"));
        assert_eq!(chosen(Vec::new(), None), None);
    }
}
