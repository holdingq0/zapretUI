// The DNS Windows itself is set to: the servers of the adapters that lead to the internet and the
// DoH addresses given for them in the system settings. A resolver for the neural networks may
// already be set there, and then the service has nothing to add.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use windows::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_MULTICAST, GAA_FLAG_SKIP_UNICAST, GetAdaptersAddresses,
    IF_TYPE_SOFTWARE_LOOPBACK, IP_ADAPTER_ADDRESSES_LH,
};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::Networking::WinSock::{AF_INET, AF_INET6, SOCKADDR, SOCKADDR_IN, SOCKADDR_IN6};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
use windows::core::HSTRING;

const DOH_KEY: &str = r"SYSTEM\CurrentControlSet\Services\Dnscache\InterfaceSpecificParameters";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct System {
    pub servers: Vec<IpAddr>,
    /// DoH addresses set for those servers, where Windows has any.
    pub doh: Vec<String>,
}

unsafe fn ip_of(addr: *const SOCKADDR) -> Option<IpAddr> {
    unsafe {
        if addr.is_null() {
            return None;
        }
        let family = (*addr).sa_family;
        if family == AF_INET {
            let v4 = &*addr.cast::<SOCKADDR_IN>();
            Some(IpAddr::V4(Ipv4Addr::from(v4.sin_addr.S_un.S_addr.to_ne_bytes())))
        } else if family == AF_INET6 {
            let v6 = &*addr.cast::<SOCKADDR_IN6>();
            Some(IpAddr::V6(Ipv6Addr::from(v6.sin6_addr.u.Byte)))
        } else {
            None
        }
    }
}

fn reg_string(key: &str, name: &str) -> Option<String> {
    let mut buf = [0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let read = unsafe {
        RegGetValueW(HKEY_LOCAL_MACHINE, &HSTRING::from(key), &HSTRING::from(name), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut size))
    };
    if read.is_err() {
        return None;
    }
    let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len])).filter(|s| !s.is_empty())
}

/// Placeholders Windows puts on adapters that were given no DNS.
fn placeholder(ip: &IpAddr) -> bool {
    matches!(ip, IpAddr::V6(v6) if v6.segments()[0] & 0xffc0 == 0xfec0)
}

pub fn read() -> System {
    let mut system = System::default();
    unsafe {
        let flags = GAA_FLAG_SKIP_UNICAST | GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_INCLUDE_GATEWAYS;
        let mut size = 32 * 1024u32;
        for _ in 0..3 {
            let mut buf = vec![0u64; size as usize / 8 + 1];
            let head = buf.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
            match GetAdaptersAddresses(0, flags, None, Some(head), &mut size) {
                0 => {
                    let mut node = head;
                    while !node.is_null() {
                        let adapter = &*node;
                        node = adapter.Next;
                        // Only what leads somewhere: virtual switches have servers and no gateway.
                        let leads = adapter.OperStatus == IfOperStatusUp
                            && adapter.IfType != IF_TYPE_SOFTWARE_LOOPBACK
                            && !adapter.FirstGatewayAddress.is_null();
                        if !leads {
                            continue;
                        }
                        let name: String = adapter.AdapterName.to_string().unwrap_or_default();
                        let mut dns = adapter.FirstDnsServerAddress;
                        while !dns.is_null() {
                            let entry = &*dns;
                            dns = entry.Next;
                            let Some(ip) = ip_of(entry.Address.lpSockaddr) else { continue };
                            if placeholder(&ip) || ip.is_loopback() || system.servers.contains(&ip) {
                                continue;
                            }
                            system.servers.push(ip);
                            let key = format!(r"{DOH_KEY}\{name}\DohInterfaceSettings\Doh\{ip}");
                            let key6 = format!(r"{DOH_KEY}\{name}\DohInterfaceSettings\Doh6\{ip}");
                            if let Some(template) = reg_string(&key, "DohTemplate").or_else(|| reg_string(&key6, "DohTemplate")) {
                                if !system.doh.contains(&template) {
                                    system.doh.push(template);
                                }
                            }
                        }
                    }
                    break;
                }
                // The buffer was too small; `size` now says how much is needed.
                111 => continue,
                _ => break,
            }
        }
    }
    system
}
