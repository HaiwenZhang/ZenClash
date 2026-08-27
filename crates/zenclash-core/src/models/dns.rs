use serde::{Deserialize, Serialize};

/// DNS record kinds supported by Mihomo's controller query endpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum DnsRecordType {
    /// IPv4 address records.
    A,
    /// IPv6 address records.
    #[serde(rename = "AAAA")]
    Aaaa,
}

impl DnsRecordType {
    /// Returns the query value accepted by Mihomo.
    #[must_use]
    pub const fn api_value(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::Aaaa => "AAAA",
        }
    }
}

/// One question echoed by Mihomo's DNS response.
#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub struct DnsQuestion {
    /// Queried DNS name.
    #[serde(default)]
    pub name: String,
    /// DNS record type number.
    #[serde(default, rename = "type")]
    pub record_type: u16,
}

/// One answer returned by Mihomo's DNS resolver.
#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub struct DnsAnswer {
    /// Answer owner name.
    #[serde(default)]
    pub name: String,
    /// DNS record type number.
    #[serde(default, rename = "type")]
    pub record_type: u16,
    /// Remaining answer lifetime in seconds.
    #[serde(default, rename = "TTL", alias = "ttl")]
    pub ttl: u32,
    /// Textual answer data.
    #[serde(default)]
    pub data: String,
}

/// Typed response from Mihomo's `/dns/query` endpoint.
#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub struct DnsQueryResponse {
    /// DNS response status code, where zero means success.
    #[serde(default)]
    pub status: u16,
    /// Whether the response was truncated.
    #[serde(default, rename = "TC")]
    pub truncated: bool,
    /// Whether recursion was requested.
    #[serde(default, rename = "RD")]
    pub recursion_desired: bool,
    /// Whether recursive resolution is available.
    #[serde(default, rename = "RA")]
    pub recursion_available: bool,
    /// Whether authenticated data was reported.
    #[serde(default, rename = "AD")]
    pub authenticated_data: bool,
    /// Whether DNSSEC checking was disabled.
    #[serde(default, rename = "CD")]
    pub checking_disabled: bool,
    /// Questions echoed by the resolver.
    #[serde(default)]
    pub question: Vec<DnsQuestion>,
    /// Resource records returned by the resolver.
    #[serde(default)]
    pub answer: Vec<DnsAnswer>,
}
