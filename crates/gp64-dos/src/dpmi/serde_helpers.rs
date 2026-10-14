//! Serde helpers for large arrays in DPMI types.

/// Serde helper for `Box<[u16; 65536]>`.
pub mod boxed_u16_65536 {
    use serde::ser::SerializeSeq;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        data: &Box<[u16; 65536]>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(65536))?;
        for item in data.iter() {
            seq.serialize_element(item)?;
        }
        seq.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Box<[u16; 65536]>, D::Error> {
        let items: Vec<u16> = Vec::deserialize(deserializer)?;
        if items.len() != 65536 {
            return Err(serde::de::Error::custom(format!(
                "expected 65536 elements, got {}",
                items.len()
            )));
        }
        let mut arr = Box::new([0u16; 65536]);
        arr.copy_from_slice(&items);
        Ok(arr)
    }
}

/// Serde helper for `[IdtEntry; 256]`.
pub mod idt_entry_256 {
    use super::super::IdtEntry;
    use serde::ser::SerializeSeq;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        data: &[IdtEntry; 256],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(256))?;
        for item in data.iter() {
            seq.serialize_element(item)?;
        }
        seq.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<[IdtEntry; 256], D::Error> {
        let items: Vec<IdtEntry> = Vec::deserialize(deserializer)?;
        if items.len() != 256 {
            return Err(serde::de::Error::custom(format!(
                "expected 256 elements, got {}",
                items.len()
            )));
        }
        let mut arr = [IdtEntry::default(); 256];
        arr.copy_from_slice(&items);
        Ok(arr)
    }
}

/// Serde helper for `[Option<(u16, u32)>; 256]`.
pub mod option_u16_u32_256 {
    use serde::ser::SerializeSeq;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        data: &[Option<(u16, u32)>; 256],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(256))?;
        for item in data.iter() {
            seq.serialize_element(item)?;
        }
        seq.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<[Option<(u16, u32)>; 256], D::Error> {
        let items: Vec<Option<(u16, u32)>> = Vec::deserialize(deserializer)?;
        if items.len() != 256 {
            return Err(serde::de::Error::custom(format!(
                "expected 256 elements, got {}",
                items.len()
            )));
        }
        let mut arr: [Option<(u16, u32)>; 256] = [None; 256];
        arr.copy_from_slice(&items);
        Ok(arr)
    }
}
