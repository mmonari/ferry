//! `ferry config get|set|schema`: the settings of `config.toml`, through ferryd,
//! which checks every value with the rules of its own file loader.

use std::io::Write;

use ferry_core::api::{
    ConfigGetParams, ConfigSchema, ConfigSetParams, ConfigSetResult, ConfigType, ConfigValues,
    Request,
};
use serde_json::Value;

use crate::cli::ConfigCommand;
use crate::client::{Client, ClientError};
use crate::{render, typed};

/// `ferry config …`. With `json` the daemon's raw result is printed, as for every read command.
pub fn run(
    client: &mut Client,
    command: &ConfigCommand,
    json: bool,
    out: &mut dyn Write,
) -> Result<(), ClientError> {
    match command {
        ConfigCommand::Get { key } => {
            let raw = client.call(Request::ConfigGet(ConfigGetParams { key: key.clone() }))?;
            if json {
                writeln!(out, "{raw}")?;
            } else {
                write!(
                    out,
                    "{}",
                    render::config_values(&typed::<ConfigValues>(raw)?, key.as_deref())
                )?;
            }
        }
        ConfigCommand::Set { key, value } => {
            // The key's type says how to read the text: `true` is a boolean for one key and a
            // name for another. A text that does not fit goes on as text, and ferryd says what
            // was expected, in its own words.
            let schema: ConfigSchema = typed(client.call(Request::ConfigSchema)?)?;
            let kind = schema.keys.iter().find(|k| &k.key == key).map(|k| k.kind);
            let raw = client.call(Request::ConfigSet(ConfigSetParams {
                key: key.clone(),
                value: parse_value(kind, value),
            }))?;
            if json {
                writeln!(out, "{raw}")?;
            } else {
                write!(
                    out,
                    "{}",
                    render::config_set(&typed::<ConfigSetResult>(raw)?)
                )?;
            }
        }
        ConfigCommand::Schema => {
            let raw = client.call(Request::ConfigSchema)?;
            if json {
                writeln!(out, "{raw}")?;
            } else {
                write!(
                    out,
                    "{}",
                    render::config_schema(&typed::<ConfigSchema>(raw)?)
                )?;
            }
        }
    }
    Ok(())
}

/// The JSON value for `text` as a key of type `kind` reads it.
pub fn parse_value(kind: Option<ConfigType>, text: &str) -> Value {
    let as_text = || Value::String(text.to_string());
    match kind {
        Some(ConfigType::Bool) => match text.trim().to_ascii_lowercase().as_str() {
            "true" | "on" | "yes" | "1" => Value::Bool(true),
            "false" | "off" | "no" | "0" => Value::Bool(false),
            _ => as_text(),
        },
        Some(ConfigType::Int) => text
            .trim()
            .parse::<i64>()
            .map_or_else(|_| as_text(), Value::from),
        _ => as_text(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn text_is_read_as_the_keys_type_wants_it() {
        let v = |kind, text| parse_value(kind, text);
        assert_eq!(v(Some(ConfigType::Bool), "true"), json!(true));
        assert_eq!(v(Some(ConfigType::Bool), "Off"), json!(false));
        assert_eq!(v(Some(ConfigType::Bool), "yes"), json!(true));
        assert_eq!(v(Some(ConfigType::Bool), "0"), json!(false));
        assert_eq!(v(Some(ConfigType::Int), "40"), json!(40));
        assert_eq!(v(Some(ConfigType::Int), " -3 "), json!(-3));
        // a name that looks like a number or a boolean is still a name
        assert_eq!(v(Some(ConfigType::Name), "123"), json!("123"));
        assert_eq!(v(Some(ConfigType::Name), "true"), json!("true"));
        assert_eq!(v(Some(ConfigType::Enum), "off"), json!("off"));
        assert_eq!(
            v(Some(ConfigType::Chord), "Super+Alt+F12"),
            json!("Super+Alt+F12")
        );
        // text that does not fit goes on as text: ferryd says what it expected
        assert_eq!(v(Some(ConfigType::Int), "forty"), json!("forty"));
        assert_eq!(v(Some(ConfigType::Int), "4.5"), json!("4.5"));
        assert_eq!(v(Some(ConfigType::Bool), "maybe"), json!("maybe"));
        // a key the schema does not have is sent as text, and ferryd answers unknown_key
        assert_eq!(v(None, "40"), json!("40"));
    }
}
