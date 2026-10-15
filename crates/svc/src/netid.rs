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

pub fn current() -> Option<Network> {
    let found = unsafe {
        let init = CoInitializeEx(None, COINIT_MULTITHREADED);
        let found = query();
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

unsafe fn query() -> windows::core::Result<Option<Network>> {
    unsafe {
        let manager: INetworkListManager = CoCreateInstance(&NetworkListManager, None, CLSCTX_ALL)?;
        let networks: IEnumNetworks = manager.GetNetworks(NLM_ENUM_NETWORK_CONNECTED)?;
        let internet = NLM_CONNECTIVITY_IPV4_INTERNET.0 | NLM_CONNECTIVITY_IPV6_INTERNET.0;
        let mut fallback: Option<Network> = None;
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
            let found = Network { id, name: network.GetName()?.to_string(), link: link_of(&network), internet: online };
            if online {
                return Ok(Some(found));
            }
            fallback.get_or_insert(found);
        }
        Ok(fallback)
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
