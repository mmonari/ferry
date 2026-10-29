//! `config.schema`, `config.get` and `config.set` .
//!
//! The keys are [`crate::config::KEYS`]: the file loader's own list, so a value `config.set`
//! accepts is a value the loader accepts, and the schema cannot drift from either.
//!
//! `config.set` changes a copy of the saved config and validates it, edits the text of
//! `config.toml` with `toml_edit` (comments, order and spacing stay as the user left them),
//! lets the loader judge that text, writes it atomically, and only then changes ferryd's memory:
//! a failure at any step changes nothing. A `live` key is applied at once; a `restart` key is
//! saved and listed in `pending` until ferryd restarts; the escape chord is matched by ferryd at
//! once and reaches the Hyprland bind with `ferry setup`.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use ferry_core::api::{
    Applies, ConfigGetParams, ConfigKey, ConfigSchema, ConfigSetParams, ConfigSetResult,
    ConfigType, ConfigValues, ErrorCode,
};
use serde_json::Value;
use toml_edit::{DocumentMut, Item};

use crate::config::{ChordKind, Config, Field, KEYS, Key, find_key};
use crate::ctl::Ctl;
use crate::fsutil::{StoreError, mode_of, read_optional, write_atomic};

/// The mode of a `config.toml` ferryd creates; an existing file keeps its own.
const NEW_FILE_MODE: u32 = 0o644;

type ApiError = ferry_core::api::ApiError;

fn error(code: ErrorCode, message: impl Into<String>, hint: Option<String>) -> ApiError {
    ApiError {
        code,
        message: message.into(),
        hint,
    }
}

fn bad_value(message: impl Into<String>, hint: Option<String>) -> ApiError {
    error(ErrorCode::BadValue, message, hint)
}

// --- the schema ---------------------------------------------------------------------------------

fn config_type(field: &Field) -> ConfigType {
    match field {
        Field::Bool { .. } => ConfigType::Bool,
        Field::Int { .. } => ConfigType::Int,
        Field::Name { .. } => ConfigType::Name,
        Field::Chord { .. } => ConfigType::Chord,
        Field::Enum { .. } => ConfigType::Enum,
    }
}

/// `config.schema`: every key, with its default (from `Config::default`, the one place it is
/// written, RG-CFG-03).
pub fn schema() -> ConfigSchema {
    let defaults = Config::default();
    ConfigSchema {
        keys: KEYS
            .iter()
            .map(|k| {
                let (min, max, slider_max) = match k.field {
                    Field::Int {
                        min,
                        max,
                        slider_max,
                        ..
                    } => (Some(min), Some(max), slider_max),
                    _ => (None, None, None),
                };
                let choices = match k.field {
                    Field::Enum { choices, .. } => {
                        Some(choices.iter().map(|c| (*c).to_string()).collect())
                    }
                    _ => None,
                };
                ConfigKey {
                    key: k.key.to_string(),
                    kind: config_type(&k.field),
                    default: k.value(&defaults),
                    min,
                    max,
                    unit: k.unit.map(str::to_string),
                    slider_max,
                    choices,
                    applies: k.applies,
                    doc: k.doc.to_string(),
                    available: k.available,
                }
            })
            .collect(),
    }
}

// --- the values ---------------------------------------------------------------------------------

/// The saved values of every key, and the keys whose saved value is not the one this ferryd runs
/// with. This is the `config.get` result and the `config` event.
pub fn values(ctl: &Ctl) -> ConfigValues {
    let s = ctl.state.borrow();
    let mut values = BTreeMap::new();
    let mut pending = Vec::new();
    for k in KEYS {
        let saved = k.value(&s.saved);
        if saved != k.value(&s.config) {
            pending.push(k.key.to_string());
        }
        values.insert(k.key.to_string(), saved);
    }
    ConfigValues { values, pending }
}

fn unknown_key(key: &str) -> ApiError {
    // `edge_resistance_px` without its section: say which key was meant
    let meant: Vec<_> = KEYS
        .iter()
        .filter(|k| k.parts().1 == key)
        .map(|k| k.key)
        .collect();
    let hint = match meant.as_slice() {
        [one] => format!("did you mean {one}? `ferry config schema` lists the keys"),
        _ => "`ferry config schema` lists the keys".to_string(),
    };
    error(
        ErrorCode::UnknownKey,
        format!("there is no config key {key:?}"),
        Some(hint),
    )
}

/// `config.get`: one key, or every key.
pub fn get(ctl: &Ctl, p: ConfigGetParams) -> Result<ConfigValues, ApiError> {
    let mut all = values(ctl);
    if let Some(key) = p.key {
        let k = find_key(&key).ok_or_else(|| unknown_key(&key))?;
        all.values.retain(|name, _| name == k.key);
        all.pending.retain(|name| name == k.key);
    }
    Ok(all)
}

// --- setting a value ----------------------------------------------------------------------------

fn range_hint(min: i64, max: i64, unit: Option<&str>) -> String {
    match unit {
        Some(u) => format!("{min} to {max} {u}"),
        None => format!("{min} to {max}"),
    }
}

/// Sets `key` to `value` in `config` and returns the value as it will be saved. A value of the
/// wrong type or outside the range is `bad_value`; the rest of the loader's rules
/// ([`Config::validate`]) are applied by the caller to the result.
pub fn apply(config: &mut Config, key: &Key, value: &Value) -> Result<Value, ApiError> {
    let wrong_type = |expected: &str| {
        bad_value(
            format!("{}: expected {expected}, got {value}", key.key),
            Some(allowed(key)),
        )
    };
    match key.field {
        Field::Bool { set, .. } => {
            let b = value.as_bool().ok_or_else(|| wrong_type("true or false"))?;
            set(config, b);
            Ok(Value::Bool(b))
        }
        Field::Int { min, max, set, .. } => {
            // a float, or a number past i64, is not a whole number in range
            let n = value.as_i64().ok_or_else(|| wrong_type("a whole number"))?;
            if n < min || n > max {
                return Err(bad_value(
                    format!("{}: {n} is out of range", key.key),
                    Some(allowed(key)),
                ));
            }
            set(config, n);
            Ok(Value::from(n))
        }
        Field::Name { set, .. } => {
            let s = value.as_str().ok_or_else(|| wrong_type("a text"))?;
            set(config, s.to_string());
            Ok(Value::from(s))
        }
        Field::Chord { kind, set, .. } => {
            let s = value.as_str().ok_or_else(|| wrong_type("a text"))?;
            // saved in the spelling `ferry setup` and the status show
            let canonical = kind
                .canonical(s)
                .map_err(|e| bad_value(format!("{}: {s:?}: {e}", key.key), Some(allowed(key))))?;
            set(config, canonical.clone());
            Ok(Value::from(canonical))
        }
        Field::Enum { set, .. } => {
            let s = value.as_str().ok_or_else(|| wrong_type("a text"))?;
            if !set(config, s) {
                return Err(bad_value(
                    format!("{}: {s:?} is not one of the choices", key.key),
                    Some(allowed(key)),
                ));
            }
            Ok(Value::from(s))
        }
    }
}

/// What the key accepts, in a few words: the hint of every refusal.
fn allowed(key: &Key) -> String {
    match key.field {
        Field::Bool { .. } => "true or false".into(),
        Field::Int { min, max, .. } => range_hint(min, max, key.unit),
        Field::Name { .. } => {
            "no whitespace, control characters or ':', at most 64 characters; empty uses the hostname"
                .into()
        }
        Field::Chord {
            kind: ChordKind::Escape,
            ..
        } => {
            "at least two modifiers (Super, Ctrl, Alt, Shift) and one key, such as Super+Ctrl+Escape"
                .into()
        }
        Field::Chord {
            kind: ChordKind::Go,
            ..
        } => "two or three modifiers (Super, Ctrl, Alt, Shift) and no key, such as Super+Ctrl+Alt"
            .into(),
        Field::Enum { choices, .. } => format!("one of {}", choices.join(", ")),
    }
}

/// `text` (the content of `config.toml`) with `key` set to `value`, and nothing else changed:
/// comments, order and spacing stay, and the comment after the old value stays after the new
/// one. A section the file lacks is added at the end.
pub fn edit_toml(text: &str, key: &Key, value: &Value) -> Result<String, ApiError> {
    let broken = |what: String| {
        error(
            ErrorCode::Internal,
            what,
            Some("fix the file by hand; ferryd does not overwrite a file it cannot read".into()),
        )
    };
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| broken(format!("config.toml is not valid TOML: {e}")))?;
    let (section, name) = key.parts();
    let had_text = !text.trim().is_empty();
    let new_section = doc.as_table().get(section).is_none();
    let item = doc
        .as_table_mut()
        .entry(section)
        .or_insert_with(toml_edit::table);
    if new_section
        && had_text
        && let Some(table) = item.as_table_mut()
    {
        // a blank line before the header, as a person would write it
        table.decor_mut().set_prefix("\n");
    }
    let Some(table) = item.as_table_like_mut() else {
        return Err(broken(format!("config.toml: [{section}] is not a table")));
    };
    let mut new = match value {
        Value::Bool(b) => toml_edit::Value::from(*b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => toml_edit::Value::from(i),
            None => {
                return Err(bad_value(
                    format!("{}: {n} is not a whole number", key.key),
                    None,
                ));
            }
        },
        Value::String(s) => toml_edit::Value::from(s.as_str()),
        other => {
            return Err(bad_value(format!("{}: cannot save {other}", key.key), None));
        }
    };
    match table.get_mut(name) {
        Some(old) => {
            if let Some(v) = old.as_value() {
                *new.decor_mut() = v.decor().clone();
            }
            *old = Item::Value(new);
        }
        None => {
            table.insert(name, Item::Value(new));
        }
    }
    Ok(doc.to_string())
}

/// The file to write: `config.toml`, or what it links to, so a dotfile link survives.
fn target_of(path: &Path) -> PathBuf {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => {
            std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
        }
        _ => path.to_path_buf(),
    }
}

fn store_error(e: StoreError) -> ApiError {
    match &e {
        StoreError::Io { path, source }
            if matches!(
                source.kind(),
                io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
            ) =>
        {
            error(
                ErrorCode::ReadOnly,
                format!("{} cannot be written: {source}", path.display()),
                Some("make the file and its directory writable, or edit it by hand; nothing was changed".into()),
            )
        }
        _ => error(
            ErrorCode::Internal,
            e.to_string(),
            Some("see the ferryd log".into()),
        ),
    }
}

/// `config.set`.
pub async fn set(ctl: &Ctl, p: ConfigSetParams) -> Result<ConfigSetResult, ApiError> {
    let key = find_key(&p.key).ok_or_else(|| unknown_key(&p.key))?;
    let path = ctl.paths.config_file.clone();

    // Everything up to the write is on copies: a refusal changes nothing.
    let mut next = ctl.state.borrow().saved.clone();
    let value = apply(&mut next, key, &p.value)?;
    if let Some((refused, why)) = next.refusal() {
        return Err(bad_value(
            format!("{}: {why}", refused.key),
            Some(allowed(refused)),
        ));
    }

    let target = target_of(&path);
    let text = read_optional(&target)
        .map_err(store_error)?
        .unwrap_or_default();
    let new_text = edit_toml(&text, key, &value)?;
    // The loader judges the text that would be saved, as it will when ferryd starts.
    let (reloaded, warnings) = Config::parse(&new_text, &path)
        .map_err(|e| bad_value(e.to_string(), Some(allowed(key))))?;
    if new_text != text {
        let mode = mode_of(&target).unwrap_or(NEW_FILE_MODE);
        write_atomic(&target, new_text.as_bytes(), mode).map_err(store_error)?;
    }

    // Saved. Now ferryd's memory: the saved config always, the one in force when the key is.
    {
        let mut s = ctl.state.borrow_mut();
        s.saved = reloaded;
        // what the loader says about the file as it is now: fixing a clamped value fixes its line
        s.config_warnings = warnings;
        if key.applies != Applies::Restart {
            apply(&mut s.config, key, &value)?;
        }
    }
    if key.parts().0 == "crossing" && ctl.capture_on {
        crate::ctl::push_source_config(ctl).await;
    }
    if key.parts().0 == "clipboard" {
        let c = ctl.state.borrow().config.clipboard.clone();
        let _ = ctl
            .net
            .send(crate::chan::NetCmd::ClipConfig {
                enabled: c.enabled,
                max_bytes: c.max_bytes,
            })
            .await;
    }
    if key.key == "notify.enabled" {
        let on = ctl.state.borrow().config.notify.enabled;
        crate::notify::set_enabled(ctl, on);
    }
    tracing::info!(key = key.key, applies = ?key.applies, "config.set");
    Ok(ConfigSetResult {
        key: key.key.to_string(),
        value,
        applies: key.applies,
    })
}

#[cfg(test)]
mod tests;
