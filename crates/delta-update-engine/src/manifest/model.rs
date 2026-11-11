use std::collections::HashMap;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Patch strategy as it appears on the wire.
///
/// This is deliberately not a closed enum: serde must accept unknown values
/// so a newer publisher can add a strategy without breaking older clients.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Strategy {
    Full,
    QbsdiffZstd,
    QbsdiffLzma2,
    HdiffZstd,
    HdiffLzma2,
    ZucchiniZstd,
    ZstdDict,
    Unknown(String),
}

impl Strategy {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Full => "full",
            Self::QbsdiffZstd => "qbsdiff+zstd",
            Self::QbsdiffLzma2 => "qbsdiff+lzma2",
            Self::HdiffZstd => "hdiff+zstd",
            Self::HdiffLzma2 => "hdiff+lzma2",
            Self::ZucchiniZstd => "zucchini+zstd",
            Self::ZstdDict => "zstd-dict",
            Self::Unknown(value) => value,
        }
    }

    pub fn is_full(&self) -> bool {
        matches!(self, Self::Full)
    }

    /// Patch container strategy id, or `None` when no `.tdp` applies.
    pub fn patch_id(&self) -> Option<u16> {
        match self {
            Self::QbsdiffZstd => Some(0x0001),
            Self::QbsdiffLzma2 => Some(0x0002),
            Self::HdiffZstd => Some(0x0003),
            Self::HdiffLzma2 => Some(0x0004),
            Self::ZucchiniZstd => Some(0x0005),
            Self::ZstdDict => Some(0x0006),
            Self::Full | Self::Unknown(_) => None,
        }
    }

    pub fn from_patch_id(id: u16) -> Option<Self> {
        match id {
            0x0001 => Some(Self::QbsdiffZstd),
            0x0002 => Some(Self::QbsdiffLzma2),
            0x0003 => Some(Self::HdiffZstd),
            0x0004 => Some(Self::HdiffLzma2),
            0x0005 => Some(Self::ZucchiniZstd),
            0x0006 => Some(Self::ZstdDict),
            _ => None,
        }
    }

    pub fn from_wire(value: &str) -> Self {
        match value {
            "full" => Self::Full,
            "qbsdiff+zstd" => Self::QbsdiffZstd,
            "qbsdiff+lzma2" => Self::QbsdiffLzma2,
            "hdiff+zstd" => Self::HdiffZstd,
            "hdiff+lzma2" => Self::HdiffLzma2,
            "zucchini+zstd" => Self::ZucchiniZstd,
            "zstd-dict" => Self::ZstdDict,
            other => Self::Unknown(other.to_string()),
        }
    }
}

impl Serialize for Strategy {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Strategy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct StrategyVisitor;

        impl Visitor<'_> for StrategyVisitor {
            type Value = Strategy;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a strategy identifier string")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(Strategy::from_wire(value))
            }
        }

        deserializer.deserialize_str(StrategyVisitor)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct Manifest {
    pub schema: u32,
    pub version: String,
    pub channel: Option<String>,
    pub published_at: Option<String>,
    pub min_client_schema: u32,
    /// Human-readable release notes shown by the application.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// The application may refuse to skip this update.
    #[serde(default, skip_serializing_if = "is_false")]
    pub mandatory: bool,
    pub files: HashMap<String, FileEntry>,
    #[serde(default)]
    pub rollback: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct FileEntry {
    pub sha256: String,
    pub size: u64,
    pub strategy: Strategy,
    pub delta_key: Option<String>,
    pub delta_size: Option<u64>,
    pub full_key: Option<String>,
    pub executable: Option<bool>,
}
