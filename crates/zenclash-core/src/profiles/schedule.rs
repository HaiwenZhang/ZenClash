use std::{collections::BTreeSet, str::FromStr};

use chrono::{DateTime, Local, Utc};
use cron::Schedule;

use super::{
    MAX_PROFILE_UPDATE_INTERVAL_MINUTES, MIN_PROFILE_UPDATE_INTERVAL_MINUTES, ProfileSource,
    ProfileStore, ProfileStoreError, ProfileStoreResult, RemoteProfileOptions,
    normalized_profile_name, normalized_remote_url, normalized_user_agent,
};

impl ProfileStore {
    /// Changes the persisted interval-based automatic-update policy.
    ///
    /// Selecting an interval clears any previously configured cron schedule.
    ///
    /// # Errors
    ///
    /// Returns an error when the profile is missing or local, the interval is
    /// outside the supported range, or the catalog cannot be persisted.
    pub fn set_update_policy(
        &self,
        id: &str,
        enabled: bool,
        interval_minutes: u32,
    ) -> ProfileStoreResult<()> {
        if !(MIN_PROFILE_UPDATE_INTERVAL_MINUTES..=MAX_PROFILE_UPDATE_INTERVAL_MINUTES)
            .contains(&interval_minutes)
        {
            return Err(ProfileStoreError::InvalidYaml(format!(
                "自动更新间隔必须在 {MIN_PROFILE_UPDATE_INTERVAL_MINUTES} 到 {MAX_PROFILE_UPDATE_INTERVAL_MINUTES} 分钟之间"
            )));
        }

        let _write_lease = self.write_access.acquire();
        let _transaction = self.transaction.lock();
        let mut catalog = self.load_unlocked()?;
        let profile = remote_profile_mut(&mut catalog.profiles, id)?;
        profile.auto_update = enabled;
        profile.update_interval_minutes = interval_minutes;
        profile.update_cron = None;
        self.save_unlocked(&catalog)
    }

    /// Atomically changes Authorization, Mihomo proxy routing, and an optional
    /// five-field cron expression for an existing online subscription.
    ///
    /// A non-empty cron expression enables automatic updates. Clearing cron
    /// keeps the existing interval and enablement state.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed cron, missing/local profiles, or a
    /// failed catalog transaction.
    pub fn set_remote_request_settings(
        &self,
        id: &str,
        name: &str,
        url: &str,
        user_agent: &str,
        options: RemoteProfileOptions,
        update_cron: Option<String>,
    ) -> ProfileStoreResult<()> {
        let name = normalized_profile_name(name)?;
        let url = normalized_remote_url(url)?;
        let user_agent = normalized_user_agent(user_agent)?;
        let update_cron = update_cron
            .map(|expression| expression.trim().to_owned())
            .filter(|expression| !expression.is_empty());
        if let Some(expression) = update_cron.as_deref() {
            parse_profile_schedule(expression)?;
        }

        let _write_lease = self.write_access.acquire();
        let _transaction = self.transaction.lock();
        let mut catalog = self.load_unlocked()?;
        let profile = remote_profile_mut(&mut catalog.profiles, id)?;
        profile.name = name;
        let ProfileSource::Remote {
            url: stored_url,
            user_agent: stored_user_agent,
            options: stored_options,
        } = &mut profile.source
        else {
            return Err(ProfileStoreError::NotFound(format!("{id} 不是在线订阅")));
        };
        *stored_url = url;
        *stored_user_agent = user_agent;
        *stored_options = options;
        if update_cron.is_some() {
            profile.auto_update = true;
        }
        profile.update_cron = update_cron;
        self.save_unlocked(&catalog)
    }
}

pub(super) fn cron_update_due(
    expression: &str,
    updated_at: u64,
    now: u64,
) -> ProfileStoreResult<bool> {
    if now <= updated_at {
        return Ok(false);
    }
    let schedule = parse_profile_schedule(expression)?;
    let updated_at = unix_to_local(updated_at)?;
    let Some(next) = schedule.after(&updated_at).next() else {
        return Err(ProfileStoreError::InvalidYaml(
            "订阅 Cron 没有下一次执行时间".into(),
        ));
    };
    let next = u64::try_from(next.timestamp()).map_err(|error| {
        ProfileStoreError::InvalidYaml(format!("订阅 Cron 时间超出范围：{error}"))
    })?;
    Ok(next <= now)
}

fn parse_profile_schedule(expression: &str) -> ProfileStoreResult<Schedule> {
    let mut fields = expression.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 5 {
        return Err(ProfileStoreError::InvalidYaml(
            "订阅 Cron 必须恰好包含 5 个字段（分 时 日 月 周）".into(),
        ));
    }
    let weekdays = cron_weekdays(fields[4]).ok_or_else(|| {
        ProfileStoreError::InvalidYaml(format!("订阅 Cron 星期无效：{}", fields[4]))
    })?;
    fields[4] = &weekdays;
    Schedule::from_str(&format!("0 {}", fields.join(" ")))
        .map_err(|error| ProfileStoreError::InvalidYaml(format!("订阅 Cron 无效：{error}")))
}

// Five-field cron uses 0/7 for Sunday; the cron crate uses 1 for Sunday.
fn cron_weekdays(expression: &str) -> Option<String> {
    let ordinal = |value: &str| match value.to_ascii_lowercase().as_str() {
        "sun" | "sunday" => Some(0),
        "mon" | "monday" => Some(1),
        "tue" | "tues" | "tuesday" => Some(2),
        "wed" | "wednesday" => Some(3),
        "thu" | "thurs" | "thursday" => Some(4),
        "fri" | "friday" => Some(5),
        "sat" | "saturday" => Some(6),
        _ => value.parse::<usize>().ok().filter(|day| *day <= 7),
    };
    let mut days = BTreeSet::new();
    for part in expression.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((range, step)) => {
                let step = step.parse::<std::num::NonZeroUsize>().ok()?.get();
                (range, step)
            }
            None => (part, 1),
        };
        let (start, end) = if matches!(range, "*" | "?") {
            (0, 7)
        } else if let Some((start, end)) = range.split_once('-') {
            (ordinal(start)?, ordinal(end)?)
        } else {
            let start = ordinal(range)?;
            (start, if part.contains('/') { 7 } else { start })
        };
        if start > end {
            return None;
        }
        for day in (start..=end).step_by(step) {
            days.insert((day % 7 + 1).to_string());
        }
    }
    Some(days.into_iter().collect::<Vec<_>>().join(","))
}

fn unix_to_local(unix_seconds: u64) -> ProfileStoreResult<DateTime<Local>> {
    let unix_seconds = i64::try_from(unix_seconds).map_err(|error| {
        ProfileStoreError::InvalidYaml(format!("订阅更新时间超出范围：{error}"))
    })?;
    DateTime::<Utc>::from_timestamp(unix_seconds, 0)
        .map(|value| value.with_timezone(&Local))
        .ok_or_else(|| ProfileStoreError::InvalidYaml("订阅更新时间无法转换".into()))
}

fn remote_profile_mut<'a>(
    profiles: &'a mut [super::ProfileRecord],
    id: &str,
) -> ProfileStoreResult<&'a mut super::ProfileRecord> {
    let profile = profiles
        .iter_mut()
        .find(|profile| profile.id == id)
        .ok_or_else(|| ProfileStoreError::NotFound(id.into()))?;
    if !matches!(&profile.source, ProfileSource::Remote { .. }) {
        return Err(ProfileStoreError::NotFound(format!("{id} 不是在线订阅")));
    }
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone as _;

    use super::*;

    #[test]
    fn five_field_weekday_numbers_match_names_in_lists_ranges_and_steps() {
        let start = Utc.with_ymd_and_hms(2026, 10, 17, 0, 0, 0).unwrap();
        for (numeric, named) in [
            ("1", "MON"),
            ("0", "SUN"),
            ("7", "SUN"),
            ("1-5", "MON-FRI"),
            ("5-7", "FRI,SAT,SUN"),
            ("0,2,7", "SUN,TUE"),
            ("*/2", "SUN,TUE,THU,SAT"),
            ("1-7/2", "MON,WED,FRI,SUN"),
            ("1/2", "MON,WED,FRI,SUN"),
            ("mon-fri/2", "MON,WED,FRI"),
            ("Monday-Friday", "MON-FRI"),
            ("Tues,Thurs,Sunday", "TUE,THU,SUN"),
            ("?", "*"),
        ] {
            let numeric = parse_profile_schedule(&format!("0 0 * * {numeric}")).unwrap();
            let named = parse_profile_schedule(&format!("0 0 * * {named}")).unwrap();
            assert_eq!(
                numeric.after(&start).take(10).collect::<Vec<_>>(),
                named.after(&start).take(10).collect::<Vec<_>>()
            );
        }
        let monday = parse_profile_schedule("0 0 * * 1").unwrap();
        assert_eq!(
            monday.after(&start).next().unwrap(),
            Utc.with_ymd_and_hms(2026, 10, 19, 0, 0, 0).unwrap()
        );
    }

    #[test]
    fn invalid_weekdays_are_rejected_without_panicking_or_shifting_days() {
        for weekday in [
            "8", "-1", "1/0", "*/0", "1//2", "1-", "FRI-MON", "0,,1", "DAY",
        ] {
            assert!(
                parse_profile_schedule(&format!("0 0 * * {weekday}")).is_err(),
                "{weekday}"
            );
        }
    }
}
