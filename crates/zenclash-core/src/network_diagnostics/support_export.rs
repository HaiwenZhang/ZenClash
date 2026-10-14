//! Strict support-safe projection and Markdown formatting of completed reports.

use super::{
    DiagnosticData, DiagnosticReport, DiagnosticRoute, DiagnosticStep, DiagnosticStepKind,
    unix_time_ms,
};

/// Marker requiring the default redacted support export policy.
#[derive(Clone, Copy, Debug, Default)]
pub struct SupportSafe;

/// A serialized diagnostic bundle safe to copy into a support request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupportBundle {
    /// Unix timestamp in milliseconds when the bundle was generated.
    pub generated_at_ms: u64,
    /// Markdown report with secrets, URLs, addresses and user paths omitted.
    pub markdown: String,
}

impl DiagnosticReport {
    /// Projects a report into a strictly allow-listed support-safe Markdown bundle.
    ///
    /// Controller addresses and secrets, DNS names and answers, public IPs,
    /// target URLs, provider names and raw error messages are intentionally not
    /// present in this representation.
    #[must_use]
    pub fn export_support_bundle(&self, _policy: SupportSafe) -> SupportBundle {
        let generated_at_ms = unix_time_ms();
        let mut markdown = format!(
            "# ZenClash 网络诊断报告\n\n- 生成时间：{}\n- 诊断开始：{}\n- 检查项目：{}\n\n## 检查结果\n\n| 项目 | 路由 | 结果 | 耗时 | 完成时间 |\n| --- | --- | --- | --- | --- |\n",
            report_time(generated_at_ms),
            report_time(self.started_at_ms),
            self.steps.len(),
        );
        let steps: Vec<_> = self.steps.iter().map(SafeStep::from).collect();
        for step in &steps {
            markdown.push_str(&format!(
                "| {} | {} | {} | {} ms | {} |\n",
                step.kind.report_label(),
                step.route.report_label(),
                step.status,
                step.duration_ms,
                report_time(step.completed_at_ms),
            ));
        }
        markdown.push_str("\n## 检查详情\n");
        for step in steps {
            markdown.push_str(&format!("\n### {}\n\n", step.kind.report_label()));
            markdown.push_str(&step.facts.map_or_else(
                || "检查失败，详细原因请在应用内查看。".into(),
                |facts| facts.report_summary(),
            ));
            markdown.push('\n');
        }
        SupportBundle {
            generated_at_ms,
            markdown,
        }
    }
}

struct SafeStep {
    kind: DiagnosticStepKind,
    route: DiagnosticRoute,
    completed_at_ms: u64,
    duration_ms: u64,
    status: &'static str,
    facts: Option<SafeFacts>,
}

impl From<&DiagnosticStep> for SafeStep {
    fn from(step: &DiagnosticStep) -> Self {
        let (status, facts) = match &step.outcome {
            Ok(data) => ("成功", Some(SafeFacts::from(data))),
            Err(_) => ("失败", None),
        };
        Self {
            kind: step.kind,
            route: step.route,
            completed_at_ms: step.completed_at_ms,
            duration_ms: step.duration_ms,
            status,
            facts,
        }
    }
}

enum SafeFacts {
    Controller {
        meta: bool,
    },
    Capture {
        system_proxy_observed: bool,
        tun_observed: bool,
    },
    Dns {
        response_status: u16,
        answer_count: usize,
        ttl_seconds: Vec<u32>,
    },
    Network {
        public_ip_available: bool,
        public_ip_failed: bool,
        successful_targets: usize,
        failed_targets: usize,
        latencies_ms: Vec<u64>,
    },
    Providers {
        provider_count: usize,
        item_count: usize,
    },
}

impl From<&DiagnosticData> for SafeFacts {
    fn from(data: &DiagnosticData) -> Self {
        match data {
            DiagnosticData::Controller(version) => Self::Controller { meta: version.meta },
            DiagnosticData::Capture(capture) => Self::Capture {
                system_proxy_observed: capture.system_proxy.value().is_some(),
                tun_observed: capture.tun.value().is_some(),
            },
            DiagnosticData::Dns(response) => Self::Dns {
                response_status: response.status,
                answer_count: response.answer.len(),
                ttl_seconds: response.answer.iter().map(|answer| answer.ttl).collect(),
            },
            DiagnosticData::Network(snapshot) => {
                let latencies_ms = snapshot
                    .latencies
                    .iter()
                    .filter_map(|result| result.latency_ms)
                    .collect::<Vec<_>>();
                Self::Network {
                    public_ip_available: snapshot.public_ip.is_some(),
                    public_ip_failed: snapshot.public_ip_error.is_some(),
                    successful_targets: latencies_ms.len(),
                    failed_targets: snapshot.latencies.len().saturating_sub(latencies_ms.len()),
                    latencies_ms,
                }
            }
            DiagnosticData::Providers(catalog) => Self::Providers {
                provider_count: catalog.providers.len(),
                item_count: catalog
                    .providers
                    .values()
                    .map(|provider| provider.proxies.len().saturating_add(provider.rule_count))
                    .sum(),
            },
        }
    }
}

impl DiagnosticStepKind {
    const fn report_label(self) -> &'static str {
        match self {
            Self::Controller => "内核控制器",
            Self::Capture => "系统代理与 TUN",
            Self::DnsA => "DNS IPv4（A）",
            Self::DnsAaaa => "DNS IPv6（AAAA）",
            Self::NetworkDirect => "直连网络",
            Self::NetworkMihomo => "代理网络",
            Self::ProxyProviders => "代理集合",
            Self::RuleProviders => "规则集合",
        }
    }
}

impl DiagnosticRoute {
    const fn report_label(self) -> &'static str {
        match self {
            Self::Controller => "内核控制器",
            Self::Local => "本地",
            Self::Direct => "直连",
            Self::Mihomo => "Mihomo 代理",
        }
    }
}

impl SafeFacts {
    fn report_summary(self) -> String {
        let yes_no = |value| if value { "是" } else { "否" };
        match self {
            Self::Controller { meta } => format!("- Meta 内核：{}", yes_no(meta)),
            Self::Capture {
                system_proxy_observed,
                tun_observed,
            } => format!(
                "- 已读取系统代理状态：{}\n- 已读取 TUN 状态：{}",
                yes_no(system_proxy_observed),
                yes_no(tun_observed),
            ),
            Self::Dns {
                response_status,
                answer_count,
                ttl_seconds,
            } => format!(
                "- DNS 响应码：{response_status}\n- 解析记录数：{answer_count}\n- TTL（秒）：{}",
                report_numbers(&ttl_seconds),
            ),
            Self::Network {
                public_ip_available,
                public_ip_failed,
                successful_targets,
                failed_targets,
                latencies_ms,
            } => format!(
                "- 已获取公网 IP：{}\n- 公网 IP 查询失败：{}\n- 延迟测试成功：{successful_targets}\n- 延迟测试失败：{failed_targets}\n- 延迟（ms）：{}",
                yes_no(public_ip_available),
                yes_no(public_ip_failed),
                report_numbers(&latencies_ms),
            ),
            Self::Providers {
                provider_count,
                item_count,
            } => format!("- 集合数：{provider_count}\n- 条目数：{item_count}",),
        }
    }
}

fn report_numbers(values: &[impl std::fmt::Display]) -> String {
    if values.is_empty() {
        "无".into()
    } else {
        values
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("、")
    }
}

fn report_time(timestamp_ms: u64) -> String {
    i64::try_from(timestamp_ms)
        .ok()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map_or_else(
            || "未知".into(),
            |time| time.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        )
}

#[cfg(test)]
mod tests;
