use super::*;
use crate::system_proxy::native::NativeMutationWitness;

struct Fixture {
    state: NativeProxySnapshot,
    commands: Vec<Vec<String>>,
    fail_after: Option<usize>,
    external_after: Option<usize>,
}

impl NetworkSetup for Fixture {
    fn capture(&mut self, _: &NativeProxySnapshot) -> MihomoResult<NativeProxySnapshot> {
        Ok(self.state.clone())
    }

    fn run(&mut self, step: &Step<'_>, service: &str) -> MihomoResult<()> {
        let args = step.arguments(service);
        let before = self.state.platform()?;
        let dictionary = editable_configuration(before.configuration.as_deref())?;
        // Emulate command effects from argv, independently of Step::predicted.
        match args[0].as_str() {
            "-setwebproxystate" => {
                set_number(&dictionary, "HTTPEnable", i32::from(args[2] == "on"))?
            }
            "-setsecurewebproxystate" => {
                set_number(&dictionary, "HTTPSEnable", i32::from(args[2] == "on"))?
            }
            "-setautoproxystate" => set_number(
                &dictionary,
                "ProxyAutoConfigEnable",
                i32::from(args[2] == "on"),
            )?,
            "-setwebproxy" | "-setsecurewebproxy" => {
                let prefix = if args[0] == "-setwebproxy" {
                    "HTTP"
                } else {
                    "HTTPS"
                };
                set_string(&dictionary, &format!("{prefix}Proxy"), &args[2])?;
                set_number(
                    &dictionary,
                    &format!("{prefix}Port"),
                    args[3].parse().unwrap(),
                )?;
                set_number(&dictionary, &format!("{prefix}Enable"), 1)?;
                // An omitted authentication default is equivalent to explicit off.
            }
            "-setautoproxyurl" => {
                set_string(&dictionary, "ProxyAutoConfigURLString", &args[2])?;
                set_number(&dictionary, "ProxyAutoConfigEnable", 1)?;
            }
            "-setproxybypassdomains" => {
                let entries = if args[2..] == ["Empty"] {
                    &[][..]
                } else {
                    &args[2..]
                };
                set_bypass(&dictionary, entries)?;
            }
            other => panic!("unexpected command: {other}"),
        }
        self.commands.push(args);
        if self.external_after == Some(self.commands.len()) {
            set_string(&dictionary, "ForeignCredential", "external private value")?;
        }
        self.state = NativeProxySnapshot::Platform(snapshot(&dictionary));
        if self.fail_after == Some(self.commands.len()) {
            return Err(verification_failure());
        }
        Ok(())
    }
}

fn snapshot(dictionary: &OwnedCf) -> ProxySnapshot {
    // SAFETY: Test fixture dictionary is a live owned CFDictionary.
    let configuration = unsafe { encode_configuration(dictionary.pointer()) }.unwrap();
    ProxySnapshot {
        service_id: "fixture-id".into(),
        summary: summary_from_configuration("Wi-Fi 测试", configuration.as_deref()).unwrap(),
        configuration,
    }
}

fn fixture() -> Fixture {
    let dictionary = editable_configuration(None).unwrap();
    set_string(&dictionary, "SOCKSProxy", "foreign.socks").unwrap();
    set_number(&dictionary, "SOCKSEnable", 1).unwrap();
    Fixture {
        state: NativeProxySnapshot::Platform(snapshot(&dictionary)),
        commands: vec![],
        fail_after: None,
        external_after: None,
    }
}

#[test]
fn manual_pac_and_disable_use_command_writes_and_keep_foreign_settings() {
    let mut backend = fixture();
    for change in [
        NativeProxyChange::Manual {
            host: "127.0.0.1",
            port: 7890,
            bypass: &["localhost".into()],
        },
        NativeProxyChange::Pac {
            url: "http://127.0.0.1:7891/proxy.pac",
        },
        NativeProxyChange::Disable,
    ] {
        let before = backend.state.clone();
        let desired = before.platform().unwrap().with_change(&change).unwrap();
        let mut witness = NativeMutationWitness::new(&before);
        let after = write_configuration(&mut backend, &before, &desired, &mut witness).unwrap();
        assert!(after.platform().unwrap().same_state(&desired).unwrap());
        let dictionary = decode_configuration(after.platform().unwrap().configuration.as_deref())
            .unwrap()
            .unwrap();
        let key = OwnedCf::string("SOCKSProxy").unwrap();
        assert_eq!(
            // SAFETY: `dictionary` is a live decoded dictionary and `key` is a
            // live CFString retained by this scope.
            unsafe { read_cf_string(CFDictionaryGetValue(dictionary.pointer(), key.pointer())) }
                .unwrap(),
            "foreign.socks"
        );
    }
    assert!(backend.commands.iter().all(|args| args[1] == "Wi-Fi 测试"));
    assert!(
        backend
            .commands
            .iter()
            .any(|args| args[0] == "-setwebproxy")
    );
    assert!(
        backend
            .commands
            .iter()
            .any(|args| args[0] == "-setautoproxyurl")
    );
}

#[test]
fn every_partial_command_failure_can_restore_the_previous_effective_state() {
    for failure in 1..=7 {
        let mut backend = fixture();
        let before = backend.state.clone();
        let desired = before
            .platform()
            .unwrap()
            .with_change(&NativeProxyChange::Manual {
                host: "127.0.0.1",
                port: 7890,
                bypass: &["localhost".into()],
            })
            .unwrap();
        backend.fail_after = Some(failure);
        let mut witness = NativeMutationWitness::new(&before);
        assert!(write_configuration(&mut backend, &before, &desired, &mut witness).is_err());
        assert!(witness.permits(&backend.state).unwrap());
        backend.fail_after = None;
        let partial = backend.state.clone();
        let mut restore_witness = NativeMutationWitness::new(&partial);
        let restored = write_configuration(
            &mut backend,
            &partial,
            before.platform().unwrap(),
            &mut restore_witness,
        )
        .unwrap();
        assert!(restored.same_state(&before).unwrap());
    }
}

#[test]
fn hidden_external_change_after_a_command_stops_writes_and_does_not_grant_rollback() {
    let mut backend = fixture();
    let before = backend.state.clone();
    let desired = before
        .platform()
        .unwrap()
        .with_change(&NativeProxyChange::Manual {
            host: "127.0.0.1",
            port: 7890,
            bypass: &[],
        })
        .unwrap();
    backend.external_after = Some(1);
    let mut witness = NativeMutationWitness::new(&before);
    assert!(write_configuration(&mut backend, &before, &desired, &mut witness).is_err());
    assert_eq!(backend.commands.len(), 1);
    assert!(!witness.permits(&backend.state).unwrap());
}

#[test]
fn authenticated_endpoint_replacement_is_rejected_before_disabling_any_proxy() {
    for credential_key in [
        "HTTPUser",
        "HTTPSUser",
        "HTTPProxyUsername",
        "HTTPProxyAuthenticated",
    ] {
        let dictionary = editable_configuration(None).unwrap();
        set_number(&dictionary, "HTTPEnable", 1).unwrap();
        if credential_key == "HTTPProxyAuthenticated" {
            set_number(&dictionary, credential_key, 1).unwrap();
        } else {
            set_string(&dictionary, credential_key, "private username").unwrap();
        }
        let before = NativeProxySnapshot::Platform(snapshot(&dictionary));
        let mut backend = fixture();
        backend.state = before.clone();
        let desired = before
            .platform()
            .unwrap()
            .with_change(&NativeProxyChange::Manual {
                host: "127.0.0.1",
                port: 7890,
                bypass: &[],
            })
            .unwrap();
        let mut witness = NativeMutationWitness::new(&before);
        let error = write_configuration(&mut backend, &before, &desired, &mut witness).unwrap_err();
        assert!(!format!("{error:?}").contains("private username"));
        assert!(backend.commands.is_empty());
        assert!(!witness.has_steps());
    }
}

#[test]
fn unchanged_configuration_runs_no_commands() {
    let mut backend = fixture();
    let before = backend.state.clone();
    let mut witness = NativeMutationWitness::new(&before);
    write_configuration(
        &mut backend,
        &before,
        before.platform().unwrap(),
        &mut witness,
    )
    .unwrap();
    assert!(backend.commands.is_empty());
}

#[test]
fn default_materialization_is_equivalent_but_changed_disabled_endpoints_are_not() {
    let empty = editable_configuration(None).unwrap();
    let before = snapshot(&empty);
    set_flags(&empty, false, false, false).unwrap();
    set_string(&empty, "HTTPProxy", "").unwrap();
    set_number(&empty, "HTTPPort", 0).unwrap();
    set_bypass(&empty, &[]).unwrap();
    assert!(before.same_state(&snapshot(&empty)).unwrap());
    set_string(&empty, "HTTPProxy", "external.cached.proxy").unwrap();
    assert!(!before.same_state(&snapshot(&empty)).unwrap());
}

#[test]
fn successful_exit_with_diagnostic_output_is_a_redacted_write_failure() {
    use std::os::unix::process::ExitStatusExt;
    let output = std::process::Output {
        status: std::process::ExitStatus::from_raw(0),
        stdout: b"Error: private-native-value".to_vec(),
        stderr: vec![],
    };
    let error = check_output("-setwebproxy", output).unwrap_err();
    assert!(!format!("{error:?} {error}").contains("private-native-value"));
}
