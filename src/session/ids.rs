use std::fmt::{Display, Formatter};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ulid::Ulid;

macro_rules! typed_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Ulid);

        impl $name {
            pub fn new() -> Self {
                Self(Ulid::new())
            }
        }

        impl Display for $name {
            fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl FromStr for $name {
            type Err = ulid::DecodeError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Ok(Self(Ulid::from_string(value)?))
            }
        }
    };
}

typed_id!(ProjectId);
typed_id!(SessionId);
typed_id!(ToolCallId);
typed_id!(ChangeId);

fn stable_ulid(input: &str) -> Ulid {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    Ulid::from_bytes(bytes)
}

impl ProjectId {
    pub fn from_stable_input(input: &str) -> Self {
        Self(stable_ulid(input))
    }
}
