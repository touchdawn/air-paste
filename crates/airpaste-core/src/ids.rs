use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use ulid::Ulid;

macro_rules! id_type {
    ($name:ident, $prefix:literal) => {
        #[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new() -> Self {
                Self(format!("{}_{}", $prefix, Ulid::new()))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl Display for $name {
            fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }
    };
}

id_type!(ClipId, "clip");
id_type!(DeviceId, "dev");
id_type!(SessionId, "sess");
id_type!(TransferToken, "tt");

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PairingCode(pub String);

impl PairingCode {
    pub fn new() -> Self {
        let raw = Ulid::new().to_string();
        Self(raw[raw.len() - 8..].to_ascii_uppercase())
    }
}

impl Default for PairingCode {
    fn default() -> Self {
        Self::new()
    }
}
