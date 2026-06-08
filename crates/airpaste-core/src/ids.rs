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
        // A short 6-digit numeric code (easy to read out / type), from a ULID's random low bits.
        let value = u128::from(Ulid::new()) % 1_000_000;
        Self(format!("{value:06}"))
    }
}

impl Default for PairingCode {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::PairingCode;

    #[test]
    fn pairing_code_is_six_digits() {
        for _ in 0..1000 {
            let code = PairingCode::new().0;
            assert_eq!(code.len(), 6, "code {code:?} is not 6 chars");
            assert!(
                code.chars().all(|c| c.is_ascii_digit()),
                "code {code:?} is not all digits"
            );
        }
    }
}
