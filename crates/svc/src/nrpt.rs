// Rules of the Windows name resolution policy table (NRPT): the names listed in them are asked from
// the DNS server the rule gives, and every other name goes to the system DNS as before. The same
// local rules `Add-DnsClientNrptRule` writes; the DNS client picks up changes to them by itself.

use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, REG_DWORD, REG_MULTI_SZ, REG_SZ, REG_VALUE_TYPE, RegDeleteTreeW, RegSetKeyValueW};
use windows::core::HSTRING;

const BASE: &str = r"SYSTEM\CurrentControlSet\Services\Dnscache\Parameters\DnsPolicyConfig";
/// Rules are kept under fixed names, so that whatever a killed service left behind is found again.
const RULES: usize = 16;
const PER_RULE: usize = 50;
/// Only the DNS servers of the rule are used for its names.
const GENERIC_DNS: u32 = 0x8;

// Not in the SDK headers, but every Windows since XP exports it; `ipconfig /flushdns` calls the same.
#[link(name = "dnsapi", kind = "raw-dylib")]
unsafe extern "system" {
    fn DnsFlushResolverCache() -> i32;
}

fn key(n: usize) -> String {
    format!(r"{BASE}\{{7A415052-4554-5549-A1D5-{n:012X}}}")
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn set(key: &str, name: &str, kind: REG_VALUE_TYPE, data: &[u8]) -> anyhow::Result<()> {
    unsafe {
        RegSetKeyValueW(HKEY_LOCAL_MACHINE, &HSTRING::from(key), &HSTRING::from(name), kind.0, Some(data.as_ptr().cast()), data.len() as u32).ok()?;
    }
    Ok(())
}

fn bytes(words: &[u16]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

fn write_rule(n: usize, names: &[String], server: &str) -> anyhow::Result<()> {
    let key = key(n);
    let mut list: Vec<u16> = names.iter().flat_map(|name| wide(name)).collect();
    list.push(0);
    set(&key, "Version", REG_DWORD, &2u32.to_le_bytes())?;
    set(&key, "Name", REG_MULTI_SZ, &bytes(&list))?;
    set(&key, "GenericDNSServers", REG_SZ, &bytes(&wide(server)))?;
    set(&key, "ConfigOptions", REG_DWORD, &GENERIC_DNS.to_le_bytes())?;
    set(&key, "IPSECCARestriction", REG_SZ, &bytes(&wide("")))?;
    set(&key, "DisplayName", REG_SZ, &bytes(&wide("zapretUI")))?;
    set(&key, "Comment", REG_SZ, &bytes(&wide("zapretUI: DNS для нейросетей")))?;
    Ok(())
}

/// Sends these domains and everything under them to `server`. Replaces the rules written before.
pub fn apply(domains: &[String], server: &str) -> anyhow::Result<()> {
    delete();
    // A name with a dot in front covers what is under the domain, one without covers the domain
    // itself. The two kinds go into rules of their own.
    let under: Vec<String> = domains.iter().map(|d| format!(".{d}")).collect();
    let chunks = under.chunks(PER_RULE).chain(domains.chunks(PER_RULE));
    let written = chunks.take(RULES).enumerate().try_for_each(|(n, names)| write_rule(n, names, server));
    if written.is_err() {
        delete();
    }
    flush();
    written
}

/// Takes the rules away: the names are asked from the system DNS again.
pub fn remove() {
    delete();
    flush();
}

fn delete() {
    for n in 0..RULES {
        unsafe {
            let _ = RegDeleteTreeW(HKEY_LOCAL_MACHINE, &HSTRING::from(key(n)));
        }
    }
}

/// Addresses remembered from before the change must not outlive it.
fn flush() {
    unsafe {
        DnsFlushResolverCache();
    }
}
