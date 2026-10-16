//! Command writes preserve the ordinary-user path used before native authorization.
//! Each step is witnessed and checked, but multiple commands are not atomic.

use super::*;
use crate::system_proxy::native::NativeMutationWitness;

const NUMBER_DEFAULTS: [&str; 7] = [
    "HTTPEnable",
    "HTTPSEnable",
    "ProxyAutoConfigEnable",
    "HTTPPort",
    "HTTPSPort",
    "HTTPProxyAuthenticated",
    "HTTPSProxyAuthenticated",
];
const STRING_DEFAULTS: [&str; 5] = [
    "HTTPProxy",
    "HTTPSProxy",
    "ProxyAutoConfigURLString",
    "HTTPUser",
    "HTTPSUser",
];

// networksetup materializes default values and cannot remove dictionary keys.
// Compare those defaults by meaning, retaining exact equality for other attributes.
pub(super) fn comparable_configuration(snapshot: &ProxySnapshot) -> MihomoResult<OwnedCf> {
    let dictionary = editable_configuration(snapshot.configuration.as_deref())?;
    for name in NUMBER_DEFAULTS {
        let key = OwnedCf::string(name)?;
        let value = unsafe { CFDictionaryGetValue(dictionary.pointer(), key.pointer()) };
        if value.is_null() {
            set_number(&dictionary, name, 0)?;
        } else if unsafe { CFGetTypeID(value) == CFBooleanGetTypeID() } {
            set_number(
                &dictionary,
                name,
                i32::from(unsafe { CFBooleanGetValue(value) } != 0),
            )?;
        }
    }
    for name in STRING_DEFAULTS {
        if !contains(&dictionary, name)? {
            set_string(&dictionary, name, "")?;
        }
    }
    if !contains(&dictionary, "ExceptionsList")? {
        set_bypass(&dictionary, &[])?;
    }
    Ok(dictionary)
}

fn contains(dictionary: &OwnedCf, name: &str) -> MihomoResult<bool> {
    let key = OwnedCf::string(name)?;
    // SAFETY: Both dictionary and key are live and type checked.
    Ok(!unsafe { CFDictionaryGetValue(dictionary.pointer(), key.pointer()) }.is_null())
}

enum Step<'a> {
    Http(bool),
    Https(bool),
    Pac(bool),
    HttpEndpoint(&'a str, u16),
    HttpsEndpoint(&'a str, u16),
    PacUrl(&'a str),
    Bypass(&'a [String]),
}

impl Step<'_> {
    fn option(&self) -> &'static str {
        match self {
            Self::Http(_) => "-setwebproxystate",
            Self::Https(_) => "-setsecurewebproxystate",
            Self::Pac(_) => "-setautoproxystate",
            Self::HttpEndpoint(..) => "-setwebproxy",
            Self::HttpsEndpoint(..) => "-setsecurewebproxy",
            Self::PacUrl(_) => "-setautoproxyurl",
            Self::Bypass(_) => "-setproxybypassdomains",
        }
    }

    fn arguments(&self, service: &str) -> Vec<String> {
        let mut args = vec![self.option().into(), service.into()];
        match self {
            Self::Http(on) | Self::Https(on) | Self::Pac(on) => {
                args.push(if *on { "on" } else { "off" }.into());
            }
            Self::HttpEndpoint(host, port) | Self::HttpsEndpoint(host, port) => {
                args.extend([(*host).into(), port.to_string(), "off".into()]);
            }
            Self::PacUrl(url) => args.push((*url).into()),
            Self::Bypass([]) => args.push("Empty".into()),
            Self::Bypass(entries) => args.extend(entries.iter().cloned()),
        }
        args
    }

    fn predicted(&self, before: &ProxySnapshot) -> MihomoResult<ProxySnapshot> {
        let dictionary = editable_configuration(before.configuration.as_deref())?;
        match self {
            Self::Http(on) => set_number(&dictionary, "HTTPEnable", i32::from(*on))?,
            Self::Https(on) => set_number(&dictionary, "HTTPSEnable", i32::from(*on))?,
            Self::Pac(on) => set_number(&dictionary, "ProxyAutoConfigEnable", i32::from(*on))?,
            Self::HttpEndpoint(host, port) | Self::HttpsEndpoint(host, port) => {
                let prefix = if matches!(self, Self::HttpEndpoint(..)) {
                    "HTTP"
                } else {
                    "HTTPS"
                };
                // networksetup cannot recover credentials from the keychain. Refuse
                // credential-bearing endpoint replacement before any command runs.
                ensure_unauthenticated(&dictionary, prefix)?;
                set_string(&dictionary, &format!("{prefix}Proxy"), host)?;
                set_number(&dictionary, &format!("{prefix}Port"), i32::from(*port))?;
                set_number(&dictionary, &format!("{prefix}Enable"), 1)?;
                set_number(&dictionary, &format!("{prefix}ProxyAuthenticated"), 0)?;
            }
            Self::PacUrl(url) => {
                set_string(&dictionary, "ProxyAutoConfigURLString", url)?;
                // Setting a URL also enables PAC, just as setting an endpoint
                // enables its HTTP/HTTPS protocol.
                set_number(&dictionary, "ProxyAutoConfigEnable", 1)?;
            }
            Self::Bypass(entries) => set_bypass(&dictionary, entries)?,
        }
        let configuration = unsafe { encode_configuration(dictionary.pointer()) }?;
        Ok(ProxySnapshot {
            service_id: before.service_id.clone(),
            summary: summary_from_configuration(&before.summary.service, configuration.as_deref())?,
            configuration,
        })
    }
}

fn ensure_unauthenticated(dictionary: &OwnedCf, prefix: &str) -> MihomoResult<()> {
    for suffix in [
        "User",
        "Password",
        "ProxyAuthenticated",
        "ProxyUsername",
        "ProxyPassword",
    ] {
        let key = OwnedCf::string(&format!("{prefix}{suffix}"))?;
        let value = unsafe { CFDictionaryGetValue(dictionary.pointer(), key.pointer()) };
        if value.is_null() {
            continue;
        }
        let zero = OwnedCf::number(0)?;
        let empty = OwnedCf::string("")?;
        let kind = unsafe { CFGetTypeID(value) };
        let harmless = (kind == unsafe { CFNumberGetTypeID() }
            && unsafe { CFEqual(value, zero.pointer()) } != 0)
            || (kind == unsafe { CFBooleanGetTypeID() }
                && unsafe { CFBooleanGetValue(value) } == 0)
            || (kind == unsafe { CFStringGetTypeID() }
                && unsafe { CFEqual(value, empty.pointer()) } != 0);
        if !harmless {
            return Err(crate::SystemProxyError::BackendUnavailable {
                backend: crate::SystemProxyBackendKind::MacOs,
                reason: crate::SystemProxyBackendUnavailable::AuthenticatedProxy,
            }
            .into());
        }
    }
    Ok(())
}

trait NetworkSetup {
    fn capture(&mut self, target: &NativeProxySnapshot) -> MihomoResult<NativeProxySnapshot>;
    fn run(&mut self, step: &Step<'_>, service: &str) -> MihomoResult<()>;
}

struct SystemNetworkSetup;

impl NetworkSetup for SystemNetworkSetup {
    fn capture(&mut self, target: &NativeProxySnapshot) -> MihomoResult<NativeProxySnapshot> {
        let actual = capture(NativeProxyTarget::Captured(target))?;
        // Commands address services by name. Reject duplicate names or a name
        // reassigned to another stable ID before handing it to networksetup.
        let named = capture(NativeProxyTarget::Service(
            &actual.platform()?.summary.service,
        ))?;
        if !actual.same_state(&named)? {
            return Err(ownership_changed());
        }
        Ok(actual)
    }

    fn run(&mut self, step: &Step<'_>, service: &str) -> MihomoResult<()> {
        let args = step.arguments(service);
        let args = args.iter().map(String::as_str).collect::<Vec<_>>();
        let output = crate::platform_command::output_typed("/usr/sbin/networksetup", &args)
            .map_err(|source| crate::SystemProxyError::NetworkSetupCommand {
                operation: step.option(),
                source,
            })?;
        check_output(step.option(), output)
    }
}

fn check_output(operation: &'static str, output: std::process::Output) -> MihomoResult<()> {
    // Some networksetup errors are printed with a successful exit status.
    // Setter success is silent; never retain output containing native values.
    if !output.status.success()
        || output.stdout.iter().any(|byte| !byte.is_ascii_whitespace())
        || output.stderr.iter().any(|byte| !byte.is_ascii_whitespace())
    {
        return Err(crate::SystemProxyError::NetworkSetupRejected {
            operation,
            status: output.status,
        }
        .into());
    }
    Ok(())
}

pub(in crate::system_proxy) fn apply(
    expected: &NativeProxySnapshot,
    change: &NativeProxyChange<'_>,
) -> NativeWriteAttempt {
    NativeWriteAttempt::run(expected, |witness| {
        let desired = expected.platform()?.with_change(change)?;
        write_configuration(&mut SystemNetworkSetup, expected, &desired, witness)
    })
}

pub(in crate::system_proxy) fn restore(
    expected: &NativeProxySnapshot,
    previous: &NativeProxySnapshot,
) -> NativeWriteAttempt {
    NativeWriteAttempt::run(expected, |witness| {
        write_configuration(
            &mut SystemNetworkSetup,
            expected,
            previous.platform()?,
            witness,
        )
    })
}

fn write_configuration(
    backend: &mut impl NetworkSetup,
    expected: &NativeProxySnapshot,
    desired: &ProxySnapshot,
    witness: &mut NativeMutationWitness,
) -> MihomoResult<NativeProxySnapshot> {
    let mut actual = backend.capture(expected)?;
    if !expected.same_state(&actual)? || !actual.platform()?.same_target(desired) {
        return Err(ownership_changed());
    }
    let before = actual.platform()?;
    let old = &before.summary;
    let new = &desired.summary;
    let mut steps = Vec::new();
    if old.enabled {
        steps.push(Step::Http(false));
    }
    if old.secure_enabled {
        steps.push(Step::Https(false));
    }
    if old.auto_enabled {
        steps.push(Step::Pac(false));
    }
    if old.server != new.server || old.port != new.port {
        steps.push(Step::HttpEndpoint(&new.server, new.port));
        steps.push(Step::Http(false));
    }
    if old.secure_server != new.secure_server || old.secure_port != new.secure_port {
        steps.push(Step::HttpsEndpoint(&new.secure_server, new.secure_port));
        steps.push(Step::Https(false));
    }
    if old.auto_url != new.auto_url {
        steps.push(Step::PacUrl(&new.auto_url));
        steps.push(Step::Pac(false));
    }
    if old.bypass != new.bypass {
        steps.push(Step::Bypass(&new.bypass));
    }
    if new.enabled {
        steps.push(Step::Http(true));
    }
    if new.secure_enabled {
        steps.push(Step::Https(true));
    }
    if new.auto_enabled {
        steps.push(Step::Pac(true));
    }

    // Validate the entire plan (including credential constraints) before writing.
    let mut planned = before.clone();
    for step in &steps {
        planned = step.predicted(&planned)?;
    }
    if !planned.same_state(desired)? {
        return Err(invalid_snapshot());
    }
    if before.same_state(desired)? {
        return Ok(actual);
    }

    for step in steps {
        let observed = backend.capture(&actual)?;
        if !actual.same_state(&observed)? {
            return Err(ownership_changed());
        }
        let predicted = NativeProxySnapshot::Platform(step.predicted(observed.platform()?)?);
        witness.record(predicted.clone());
        backend.run(&step, &observed.platform()?.summary.service)?;
        actual = backend.capture(&observed)?;
        if !predicted.same_state(&actual)? {
            return Err(verification_failure());
        }
    }
    if !actual.platform()?.same_state(desired)? {
        return Err(verification_failure());
    }
    Ok(actual)
}

#[cfg(test)]
mod tests;
