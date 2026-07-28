use serde::{Deserialize, Serialize};
use std::{fmt, path::Path};

pub const CHANNEL_ENV: &str = "WREC_CHANNEL";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Dev,
    Nightly,
    Release,
}

impl Channel {
    pub fn current() -> Self {
        std::env::var(CHANNEL_ENV)
            .ok()
            .and_then(|value| value.parse().ok())
            .or_else(|| {
                std::env::current_exe()
                    .ok()
                    .as_deref()
                    .and_then(Self::from_path)
            })
            .unwrap_or(Self::build_default())
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Nightly => "nightly",
            Self::Release => "release",
        }
    }

    pub const fn app_name(self) -> &'static str {
        match self {
            Self::Dev => "Wrec Dev",
            Self::Nightly => "Wrec Nightly",
            Self::Release => "Wrec",
        }
    }

    pub const fn bundle_id(self) -> &'static str {
        match self {
            Self::Dev => "app.wrec.dev",
            Self::Nightly => "app.wrec.nightly",
            Self::Release => "app.wrec.mac",
        }
    }

    pub const fn home_dir_name(self) -> &'static str {
        match self {
            Self::Dev => ".wrec-dev",
            Self::Nightly => ".wrec-nightly",
            Self::Release => ".wrec",
        }
    }

    pub const fn runtime_dir_name(self) -> &'static str {
        match self {
            Self::Dev => "wrec-dev",
            Self::Nightly => "wrec-nightly",
            Self::Release => "wrec",
        }
    }

    pub const fn cli_name(self) -> &'static str {
        match self {
            Self::Dev => "wrec-dev",
            Self::Nightly => "wrec-nightly",
            Self::Release => "wrec",
        }
    }

    pub fn notification_name(self, event: &str) -> String {
        format!("app.wrec.{}.{event}", self.as_str())
    }

    const fn build_default() -> Self {
        if cfg!(debug_assertions) {
            Self::Dev
        } else {
            Self::Release
        }
    }

    fn from_path(path: &Path) -> Option<Self> {
        path.ancestors().find_map(|component| {
            let name = component.file_name()?.to_str()?.to_ascii_lowercase();
            if name == "wrec dev.app" || name == "wrec-dev" {
                Some(Self::Dev)
            } else if name == "wrec nightly.app" || name == "wrec-nightly" {
                Some(Self::Nightly)
            } else {
                None
            }
        })
    }
}

impl std::str::FromStr for Channel {
    type Err = ParseChannelError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "dev" => Ok(Self::Dev),
            "nightly" => Ok(Self::Nightly),
            "release" | "stable" => Ok(Self::Release),
            _ => Err(ParseChannelError),
        }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParseChannelError;

impl fmt::Display for ParseChannelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("expected dev, nightly, or release")
    }
}

impl std::error::Error for ParseChannelError {}

#[cfg(test)]
mod tests {
    use super::Channel;
    use std::path::Path;

    #[test]
    fn identities_are_isolated() {
        let channels = [Channel::Dev, Channel::Nightly, Channel::Release];
        assert_eq!(
            channels.map(Channel::home_dir_name),
            [".wrec-dev", ".wrec-nightly", ".wrec"]
        );
        assert_eq!(
            channels.map(Channel::app_name),
            ["Wrec Dev", "Wrec Nightly", "Wrec"]
        );
        assert_eq!(
            channels.map(Channel::cli_name),
            ["wrec-dev", "wrec-nightly", "wrec"]
        );
    }

    #[test]
    fn packaged_paths_identify_non_release_channels() {
        assert_eq!(
            Channel::from_path(Path::new(
                "/Applications/Wrec Nightly.app/Contents/MacOS/daemon"
            )),
            Some(Channel::Nightly)
        );
        assert_eq!(
            Channel::from_path(Path::new("/tmp/Wrec Dev.app/Contents/MacOS/wrec-app")),
            Some(Channel::Dev)
        );
        assert_eq!(
            Channel::from_path(Path::new("/usr/local/lib/wrec-nightly/daemon")),
            Some(Channel::Nightly)
        );
    }
}
