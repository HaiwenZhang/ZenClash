use super::AppPreferences;

/// Discards removed settings only when the current schema rejects the file.
pub(super) fn decode_preferences(bytes: &[u8]) -> serde_json::Result<AppPreferences> {
    let error = match serde_json::from_slice(bytes) {
        Ok(preferences) => return Ok(preferences),
        Err(error) => error,
    };
    validate_unique_fields(bytes)?;
    let mut value: serde_json::Value = serde_json::from_slice(bytes)?;
    let Some(root) = value.as_object_mut() else {
        return Err(error);
    };
    let mut migrated = false;
    if matches!(
        root.get("tray_display").and_then(serde_json::Value::as_str),
        Some("traffic" | "icon")
    ) {
        root.remove("tray_display");
        migrated = true;
    }
    if root.get("core_kind").and_then(serde_json::Value::as_str) == Some("meow") {
        root.insert("core_kind".into(), "mihomo".into());
        migrated = true;
    }
    if root
        .get("last_known_good_core")
        .and_then(serde_json::Value::as_str)
        == Some("meow")
    {
        root.insert("last_known_good_core".into(), serde_json::Value::Null);
        root.insert("last_known_good_binary".into(), serde_json::Value::Null);
        migrated = true;
    }
    if let Some(binaries) = root
        .get_mut("core_binaries")
        .and_then(serde_json::Value::as_object_mut)
    {
        migrated |= binaries.remove("meow").is_some();
    }
    if migrated {
        serde_json::from_value(value)
    } else {
        Err(error)
    }
}

// Value normalizes object keys, so reject duplicates before migrating an old document.
fn validate_unique_fields(bytes: &[u8]) -> serde_json::Result<()> {
    use serde::Deserialize;
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    UniqueFields::deserialize(&mut decoder)?;
    decoder.end()
}

struct UniqueFields;

impl<'de> serde::Deserialize<'de> for UniqueFields {
    fn deserialize<D: serde::Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        decoder.deserialize_any(Self)
    }
}

impl<'de> serde::de::Visitor<'de> for UniqueFields {
    type Value = Self;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("JSON with unique object fields")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self, A::Error> {
        let mut fields = std::collections::HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !fields.insert(key.clone()) {
                return Err(serde::de::Error::custom(format!(
                    "duplicate JSON field `{key}`"
                )));
            }
            map.next_value::<Self>()?;
        }
        Ok(Self)
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut sequence: A) -> Result<Self, A::Error> {
        while sequence.next_element::<Self>()?.is_some() {}
        Ok(Self)
    }

    fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self, E> {
        Ok(Self)
    }
    fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self, E> {
        Ok(Self)
    }
    fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self, E> {
        Ok(Self)
    }
    fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self, E> {
        Ok(Self)
    }
    fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Self, E> {
        Ok(Self)
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<Self, E> {
        Ok(Self)
    }
}
