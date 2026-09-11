pub(crate) fn mihomo_version(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        if words.next()? != "Mihomo" || words.next()? != "Meta" {
            return None;
        }
        words.next().map(str::to_owned)
    })
}

#[cfg(test)]
mod tests {
    use super::mihomo_version;

    #[test]
    fn reads_actual_release_or_alpha_version_without_platform_details() {
        assert_eq!(
            mihomo_version(
                "Mihomo Meta v1.19.30 darwin arm64 with go1.26\nBuild tags: with_gvisor"
            ),
            Some("v1.19.30".into())
        );
        assert_eq!(
            mihomo_version("Mihomo Meta alpha-abc123 windows amd64\r\n"),
            Some("alpha-abc123".into())
        );
    }

    #[test]
    fn rejects_other_cores_and_missing_version() {
        for output in ["", "Mihomo Meta", "Meow Meta v1.0", "v1.19.30"] {
            assert_eq!(mihomo_version(output), None);
        }
    }
}
