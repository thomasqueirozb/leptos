#![forbid(unsafe_code)]

pub mod errors;

use crate::errors::LeptosConfigError;
use config::{Config, File, FileFormat};
use regex::Regex;
use std::convert::TryFrom;
use std::fs;
use std::{env::VarError, net::SocketAddr, str::FromStr};
use typed_builder::TypedBuilder;

/// A Struct to allow us to parse LeptosOptions from the file. Not really needed, most interactions should
/// occur with LeptosOptions
#[derive(Clone, Debug, serde::Deserialize)]
pub struct ConfFile {
    pub leptos_options: LeptosOptions,
}

/// This struct serves as a convenient place to store details used for configuring Leptos.
/// It's used in our actix and axum integrations to generate the
/// correct path for WASM, JS, and Websockets, as well as other configuration tasks.
/// It shares keys with cargo-leptos, to allow for easy interoperability
#[derive(TypedBuilder, Debug, Clone, serde::Deserialize)]
pub struct LeptosOptions {
    /// The name of the WASM and JS files generated by wasm-bindgen. Defaults to the crate name with underscores instead of dashes
    #[builder(setter(into))]
    pub output_name: String,
    /// The path of the all the files generated by cargo-leptos. This defaults to '.' for convenience when integrating with other
    /// tools.
    #[builder(setter(into), default=".".to_string())]
    pub site_root: String,
    /// The path of the WASM and JS files generated by wasm-bindgen from the root of your app
    /// By default, wasm-bindgen puts them in `pkg`.
    #[builder(setter(into), default="pkg".to_string())]
    pub site_pkg_dir: String,
    /// Used to configure the running environment of Leptos. Can be used to load dev constants and keys v prod, or change
    /// things based on the deployment environment
    /// I recommend passing in the result of `env::var("LEPTOS_ENV")`
    #[builder(setter(into), default=Env::DEV)]
    pub env: Env,
    /// Provides a way to control the address leptos is served from.
    /// Using an env variable here would allow you to run the same code in dev and prod
    /// Defaults to `127.0.0.1:3000`
    #[builder(setter(into), default=SocketAddr::from(([127,0,0,1], 3000)))]
    pub site_address: SocketAddr,
    /// The port the Websocket watcher listens on. Should match the `reload_port` in cargo-leptos(if using).
    /// Defaults to `3001`
    #[builder(default = 3001)]
    pub reload_port: u32,
}

impl LeptosOptions {
    fn try_from_env() -> Result<Self, LeptosConfigError> {
        Ok(LeptosOptions {
            output_name: std::env::var("LEPTOS_OUTPUT_NAME")
                .map_err(|e| LeptosConfigError::EnvVarError(format!("LEPTOS_OUTPUT_NAME: {e}")))?,
            site_root: env_w_default("LEPTOS_SITE_ROOT", "target/site")?,
            site_pkg_dir: env_w_default("LEPTOS_SITE_PKG_DIR", "pkg")?,
            env: Env::default(),
            site_address: env_w_default("LEPTOS_SITE_ADDR", "127.0.0.1:3000")?.parse()?,
            reload_port: env_w_default("LEPTOS_RELOAD_PORT", "3001")?.parse()?,
        })
    }
}

fn env_w_default(key: &str, default: &str) -> Result<String, LeptosConfigError> {
    match std::env::var(key) {
        Ok(val) => Ok(val),
        Err(VarError::NotPresent) => Ok(default.to_string()),
        Err(e) => Err(LeptosConfigError::EnvVarError(format!("{key}: {e}"))),
    }
}

/// An enum that can be used to define the environment Leptos is running in.
/// Setting this to the `PROD` variant will not include the WebSocket code for `cargo-leptos` watch mode.
/// Defaults to `DEV`.
#[derive(Debug, Clone, serde::Deserialize)]
pub enum Env {
    PROD,
    DEV,
}

impl Default for Env {
    fn default() -> Self {
        Self::DEV
    }
}

impl FromStr for Env {
    type Err = ();
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let sanitized = input.to_lowercase();
        match sanitized.as_ref() {
            "dev" => Ok(Self::DEV),
            "development" => Ok(Self::DEV),
            "prod" => Ok(Self::PROD),
            "production" => Ok(Self::PROD),
            _ => Ok(Self::DEV),
        }
    }
}

impl From<&str> for Env {
    fn from(str: &str) -> Self {
        let sanitized = str.to_lowercase();
        match sanitized.as_str() {
            "dev" => Self::DEV,
            "development" => Self::DEV,
            "prod" => Self::PROD,
            "production" => Self::PROD,
            _ => {
                panic!("Env var is not recognized. Maybe try `dev` or `prod`")
            }
        }
    }
}
impl From<&Result<String, VarError>> for Env {
    fn from(input: &Result<String, VarError>) -> Self {
        match input {
            Ok(str) => {
                let sanitized = str.to_lowercase();
                match sanitized.as_ref() {
                    "dev" => Self::DEV,
                    "development" => Self::DEV,
                    "prod" => Self::PROD,
                    "production" => Self::PROD,
                    _ => {
                        panic!("Env var is not recognized. Maybe try `dev` or `prod`")
                    }
                }
            }
            Err(_) => Self::DEV,
        }
    }
}

impl TryFrom<String> for Env {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        match s.to_lowercase().as_str() {
            "dev" => Ok(Self::DEV),
            "development" => Ok(Self::DEV),
            "prod" => Ok(Self::PROD),
            "production" => Ok(Self::PROD),
            other => Err(format!(
                "{other} is not a supported environment. Use either `dev` or `production`."
            )),
        }
    }
}

/// Loads [LeptosOptions] from a Cargo.toml with layered overrides. If an env var is specified, like `LEPTOS_ENV`,
/// it will override a setting in the file. It takes in an optional path to a Cargo.toml file. If None is provided,
/// you'll need to set the options as environment variables or rely on the defaults. This is the preferred
/// approach for cargo-leptos. If Some("./Cargo.toml") is provided, Leptos will read in the settings itself. This
/// option currently does not allow dashes in file or foldernames, as all dashes become underscores
pub async fn get_configuration(path: Option<&str>) -> Result<ConfFile, LeptosConfigError> {
    if let Some(path) = path {
        let text = fs::read_to_string(path).map_err(|_| LeptosConfigError::ConfigNotFound)?;

        let re: Regex = Regex::new(r#"(?m)^\[package.metadata.leptos\]"#).unwrap();
        let start = match re.find(&text) {
            Some(found) => found.start(),
            None => return Err(LeptosConfigError::ConfigSectionNotFound),
        };

        // so that serde error messages have right line number
        let newlines = text[..start].matches('\n').count();
        let input = "\n".repeat(newlines) + &text[start..];
        let toml = input
            .replace("[package.metadata.leptos]", "[leptos_options]")
            .replace('-', "_");
        let settings = Config::builder()
            // Read the "default" configuration file
            .add_source(File::from_str(&toml, FileFormat::Toml))
            // Layer on the environment-specific values.
            // Add in settings from environment variables (with a prefix of LEPTOS and '_' as separator)
            // E.g. `LEPTOS_RELOAD_PORT=5001 would set `LeptosOptions.reload_port`
            .add_source(config::Environment::with_prefix("LEPTOS").separator("_"))
            .build()?;

        settings
            .try_deserialize()
            .map_err(|e| LeptosConfigError::ConfigError(e.to_string()))
    } else {
        Ok(ConfFile {
            leptos_options: LeptosOptions::try_from_env()?,
        })
    }
}
