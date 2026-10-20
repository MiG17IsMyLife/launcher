//! 本机网卡与 IPv4 地址枚举（iphlpapi GetAdaptersAddresses）：
//! 为网络设置页提供「网卡 → 该网卡 IPv4 地址/网段」两级选择数据。

use windows::Win32::Foundation::ERROR_BUFFER_OVERFLOW;
use windows::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_INCLUDE_PREFIX, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER,
    GAA_FLAG_SKIP_MULTICAST, GET_ADAPTERS_ADDRESSES_FLAGS, GetAdaptersAddresses,
    IP_ADAPTER_ADDRESSES_LH, IP_ADAPTER_UNICAST_ADDRESS_LH,
};
use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};

/// 网卡上的一个 IPv4 单播地址
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IpInfo {
    /// 点分十进制，如 "192.168.100.114"
    pub ip: String,
    /// 子网前缀长度（0-32）
    pub prefix: u8,
}

impl IpInfo {
    /// 所在网段 CIDR，如 "192.168.100.0/24"
    #[allow(dead_code)]
    pub fn cidr(&self) -> String {
        network_cidr(self)
    }
}

/// 一张网卡（网络适配器）——列表中仅包含已连接（IfOperStatusUp）的网卡
#[derive(Clone, Debug)]
pub struct AdapterInfo {
    /// AdapterName（GUID 字符串），跨重启稳定，用作持久化标识
    pub guid: String,
    /// FriendlyName，如 "以太网"
    pub friendly: String,
    /// 硬件描述，如 "Realtek PCIe GbE Family Controller"
    pub description: String,
    /// 该网卡的 IPv4 单播地址
    pub ips: Vec<IpInfo>,
}

impl AdapterInfo {
    /// 下拉框标签，如 "以太网（Realtek PCIe GbE Family Controller）"
    pub fn label(&self) -> String {
        if self.description.is_empty() {
            self.friendly.clone()
        } else {
            format!("{}（{}）", self.friendly, self.description)
        }
    }
}

/// 前缀长度 → 32 位子网掩码
fn mask_bits(prefix: u8) -> u32 {
    if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix.min(32)) as u32
    }
}

/// IP + 前缀 → 所在网段 CIDR，如 "192.168.100.114/24" → "192.168.100.0/24"
pub fn network_cidr(ip: &IpInfo) -> String {
    match parse_ipv4(&ip.ip) {
        Some(addr) => format!(
            "{}/{}",
            format_ipv4((addr & mask_bits(ip.prefix)).to_be_bytes()),
            ip.prefix
        ),
        // 非法 IP（不应发生）时退回原样展示
        None => format!("{}/{}", ip.ip, ip.prefix),
    }
}

fn format_ipv4(b: [u8; 4]) -> String {
    format!("{}.{}.{}.{}", b[0], b[1], b[2], b[3])
}

/// 取某 IPv4 的「下一个」地址（末位八位组 +1），作为无第二个地址时的
/// ServerBox IP 默认值；末位 ≥254（广播/保留）或解析失败时返回 None。
pub fn next_ip(ip: &str) -> Option<String> {
    let addr = parse_ipv4(ip)?;
    let mut bytes = addr.to_be_bytes();
    if bytes[3] >= 254 {
        return None;
    }
    bytes[3] += 1;
    Some(format_ipv4(bytes))
}

fn parse_ipv4(s: &str) -> Option<u32> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut bytes = [0u8; 4];
    for (b, p) in bytes.iter_mut().zip(&parts) {
        *b = p.parse().ok()?;
    }
    Some(u32::from_be_bytes(bytes))
}

/// 网段标签列表（CIDR、去重保序）：同一网卡多个地址常属同一网段，下拉框按网段去重展示
pub fn subnet_labels(ips: &[IpInfo]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for ip in ips {
        let cidr = network_cidr(ip);
        if !out.contains(&cidr) {
            out.push(cidr);
        }
    }
    out
}

/// 枚举已连接（IfOperStatusUp）且有 IPv4 栈的网卡（跳过环回），按名称排序。
pub fn list_adapters() -> Vec<AdapterInfo> {
    let mut adapters = Vec::new();
    let flags = GET_ADAPTERS_ADDRESSES_FLAGS(
        GAA_FLAG_SKIP_ANYCAST.0
            | GAA_FLAG_SKIP_MULTICAST.0
            | GAA_FLAG_SKIP_DNS_SERVER.0
            | GAA_FLAG_INCLUDE_PREFIX.0,
    );
    let family = AF_INET.0 as u32;

    // 先探测所需缓冲区大小（size 传 0、缓冲区为 None，必返回 ERROR_BUFFER_OVERFLOW 并写入所需大小），
    // 再取数据；期间网卡集合变化会再次溢出，允许重试。
    let mut ok = false;
    let mut size: u32 = 0;
    let mut buf: Vec<u8> = Vec::new();
    for _ in 0..4 {
        if size == 0 {
            let rc = unsafe { GetAdaptersAddresses(family, flags, None, None, &mut size) };
            if rc != ERROR_BUFFER_OVERFLOW.0 {
                return adapters;
            }
        }
        buf.resize(size as usize, 0);
        let rc = unsafe {
            GetAdaptersAddresses(
                family,
                flags,
                None,
                Some(buf.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>()),
                &mut size,
            )
        };
        if rc == 0 {
            ok = true;
            break;
        }
        if rc != ERROR_BUFFER_OVERFLOW.0 {
            return adapters;
        }
        // 缓冲区仍不够：作废当前大小，下一轮重新探测
        size = 0;
    }
    if !ok {
        return adapters;
    }

    let mut cur = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
    while !cur.is_null() {
        let aa = unsafe { &*cur };
        cur = aa.Next;
        // 24 = IF_TYPE_SOFTWARE_LOOPBACK（环回接口对网卡选择无意义）；
        // OperStatus 1 = IfOperStatusUp，只保留已连接的网卡
        if aa.IfType == 24 || aa.OperStatus.0 != 1 {
            continue;
        }
        let mut ips = Vec::new();
        let mut unicast = aa.FirstUnicastAddress;
        while !unicast.is_null() {
            let ua: &IP_ADAPTER_UNICAST_ADDRESS_LH = unsafe { &*unicast };
            unicast = ua.Next;
            if let Some(info) = unicast_ipv4(ua) {
                ips.push(info);
            }
        }
        adapters.push(AdapterInfo {
            guid: pstr_to_string(aa.AdapterName),
            friendly: pwstr_to_string(aa.FriendlyName),
            description: pwstr_to_string(aa.Description),
            ips,
        });
    }

    adapters.sort_by(|a, b| a.friendly.to_lowercase().cmp(&b.friendly.to_lowercase()));
    adapters
}

fn unicast_ipv4(ua: &IP_ADAPTER_UNICAST_ADDRESS_LH) -> Option<IpInfo> {
    let sa = ua.Address.lpSockaddr;
    if sa.is_null() {
        return None;
    }
    unsafe {
        if (*sa).sa_family != AF_INET {
            return None;
        }
        let sin = &*(sa as *const SOCKADDR_IN);
        // S_addr 按网络字节序存放在内存中；小端机器上读成的 u32 需先 from_be 转回主机序
        let addr = u32::from_be(sin.sin_addr.S_un.S_addr);
        Some(IpInfo {
            ip: format_ipv4(addr.to_be_bytes()),
            prefix: ua.OnLinkPrefixLength,
        })
    }
}

fn pwstr_to_string(p: windows::core::PWSTR) -> String {
    if p.0.is_null() {
        return String::new();
    }
    unsafe {
        let mut len = 0usize;
        while *p.0.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p.0, len))
    }
}

fn pstr_to_string(p: windows::core::PSTR) -> String {
    if p.0.is_null() {
        return String::new();
    }
    unsafe {
        std::ffi::CStr::from_ptr(p.0.cast())
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cidr_covers_prefix_edges() {
        // 边界前缀：/0 /8 /30 /32（掩码换算经 network_cidr 间接验证）
        assert_eq!(
            network_cidr(&IpInfo {
                ip: "192.168.100.114".into(),
                prefix: 0
            }),
            "0.0.0.0/0"
        );
        assert_eq!(
            network_cidr(&IpInfo {
                ip: "10.255.3.7".into(),
                prefix: 8
            }),
            "10.0.0.0/8"
        );
        assert_eq!(
            network_cidr(&IpInfo {
                ip: "192.168.1.6".into(),
                prefix: 30
            }),
            "192.168.1.4/30"
        );
        assert_eq!(
            network_cidr(&IpInfo {
                ip: "1.2.3.4".into(),
                prefix: 32
            }),
            "1.2.3.4/32"
        );
    }

    #[test]
    fn cidr_from_ip_and_prefix() {
        assert_eq!(
            network_cidr(&IpInfo {
                ip: "192.168.100.114".into(),
                prefix: 24
            }),
            "192.168.100.0/24"
        );
        assert_eq!(
            network_cidr(&IpInfo {
                ip: "10.255.3.7".into(),
                prefix: 8
            }),
            "10.0.0.0/8"
        );
        assert_eq!(
            network_cidr(&IpInfo {
                ip: "172.16.5.9".into(),
                prefix: 20
            }),
            "172.16.0.0/20"
        );
    }

    #[test]
    fn next_ip_increments_last_octet() {
        assert_eq!(next_ip("192.168.100.5").as_deref(), Some("192.168.100.6"));
        assert_eq!(next_ip("10.0.0.1").as_deref(), Some("10.0.0.2"));
        // 广播 / 保留尾址不自动递增
        assert_eq!(next_ip("192.168.100.254"), None);
        assert_eq!(next_ip("192.168.100.255"), None);
        assert_eq!(next_ip("not-an-ip"), None);
    }

    #[test]
    fn subnet_labels_dedupe() {
        let ips = vec![
            IpInfo {
                ip: "192.168.100.114".into(),
                prefix: 24,
            },
            // 同网段的第二个地址应被去重
            IpInfo {
                ip: "192.168.100.55".into(),
                prefix: 24,
            },
            IpInfo {
                ip: "10.1.2.3".into(),
                prefix: 8,
            },
        ];
        assert_eq!(subnet_labels(&ips), vec!["192.168.100.0/24", "10.0.0.0/8"]);
        assert_eq!(ips[0].cidr(), "192.168.100.0/24");
    }

    #[test]
    fn adapter_label_combines_friendly_parts() {
        let ad = AdapterInfo {
            guid: "g".into(),
            friendly: "以太网".into(),
            description: "Realtek PCIe".into(),
            ips: Vec::new(),
        };
        assert_eq!(ad.label(), "以太网（Realtek PCIe）");
        let no_desc = AdapterInfo {
            description: String::new(),
            ..ad
        };
        assert_eq!(no_desc.label(), "以太网");
    }

    /// 真机冒烟：枚举应成功返回，且每个地址都能解析回合法 IPv4
    #[test]
    fn list_adapters_smoke() {
        let adapters = list_adapters();
        for a in &adapters {
            assert!(!a.guid.is_empty(), "网卡 GUID 不应为空");
            assert!(!a.friendly.is_empty(), "网卡 FriendlyName 不应为空");
            for ip in &a.ips {
                assert!(parse_ipv4(&ip.ip).is_some(), "非法 IPv4: {}", ip.ip);
                assert!(ip.prefix <= 32);
                assert!(network_cidr(ip).contains('/'));
            }
        }
    }
}
