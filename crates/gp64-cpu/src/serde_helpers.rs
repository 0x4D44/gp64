//! Serde helpers for arrays larger than 32 elements.
//!
//! Serde's built-in array support only covers arrays up to 32 elements.
//! These modules provide serialize/deserialize for larger fixed-size arrays.

/// Serde support for `[u8; 108]` (x87 save area).
///
/// Both directions use a fixed-arity tuple representation. Deserialization
/// pulls exactly 108 elements and rejects a 109th immediately, so it never
/// allocates a buffer sized from a snapshot's declared length. The previous
/// implementation deserialized into an unbounded `Vec<u8>` and only checked
/// the length afterwards, letting a malformed CPU snapshot force a large
/// speculative allocation before rejection (MDD-BUG-CRUCIBLE-00029).
pub mod array_108 {
    use core::fmt;
    use serde::de::{self, SeqAccess, Visitor};
    use serde::ser::SerializeTuple;
    use serde::{Deserializer, Serializer};

    const LEN: usize = 108;

    pub fn serialize<S: Serializer>(data: &[u8; LEN], serializer: S) -> Result<S::Ok, S::Error> {
        let mut tup = serializer.serialize_tuple(LEN)?;
        for byte in data {
            tup.serialize_element(byte)?;
        }
        tup.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; LEN], D::Error> {
        struct Array108Visitor;

        impl<'de> Visitor<'de> for Array108Visitor {
            type Value = [u8; LEN];

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, "an array of exactly {LEN} bytes")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut arr = [0u8; LEN];
                for (i, slot) in arr.iter_mut().enumerate() {
                    *slot = seq
                        .next_element()?
                        .ok_or_else(|| de::Error::invalid_length(i, &self))?;
                }
                // Reject a 109th element immediately (self-describing formats).
                // Never allocate past the fixed 108-element window.
                if seq.next_element::<u8>()?.is_some() {
                    return Err(de::Error::invalid_length(LEN + 1, &self));
                }
                Ok(arr)
            }
        }

        deserializer.deserialize_tuple(LEN, Array108Visitor)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use serde::de::{self, DeserializeSeed, IntoDeserializer, SeqAccess, Visitor};
    use serde::{Deserialize, Serialize};

    /// Minimal wrapper so the `#[serde(with = ...)]` helper can be exercised
    /// through a real (de)serialization round-trip.
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Wrapper {
        #[serde(with = "super::array_108")]
        data: [u8; 108],
    }

    struct OversizedTupleProbe<'a> {
        next_element_calls: &'a Cell<usize>,
    }

    impl<'de> serde::Deserializer<'de> for OversizedTupleProbe<'_> {
        type Error = de::value::Error;

        fn deserialize_any<V>(self, _visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            Err(de::Error::custom("array_108 must request a fixed tuple"))
        }

        fn deserialize_seq<V>(self, _visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            Err(de::Error::custom(
                "array_108 must not use unbounded sequence deserialization",
            ))
        }

        fn deserialize_tuple<V>(self, len: usize, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            if len != 108 {
                return Err(de::Error::custom(format!(
                    "expected 108-byte tuple request, got {len}"
                )));
            }
            visitor.visit_seq(OversizedSeq {
                next_element_calls: self.next_element_calls,
                produced: 0,
            })
        }

        serde::forward_to_deserialize_any! {
            bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
            bytes byte_buf option unit unit_struct newtype_struct tuple_struct map
            struct enum identifier ignored_any
        }
    }

    struct OversizedSeq<'a> {
        next_element_calls: &'a Cell<usize>,
        produced: usize,
    }

    impl<'de> SeqAccess<'de> for OversizedSeq<'_> {
        type Error = de::value::Error;

        fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>, Self::Error>
        where
            T: DeserializeSeed<'de>,
        {
            self.next_element_calls
                .set(self.next_element_calls.get() + 1);
            self.produced += 1;
            if self.produced <= 109 {
                seed.deserialize(1u8.into_deserializer()).map(Some)
            } else {
                Ok(None)
            }
        }

        fn size_hint(&self) -> Option<usize> {
            Some(usize::MAX)
        }
    }

    #[test]
    fn round_trips_exactly_108_bytes() {
        let mut data = [0u8; 108];
        for (i, b) in data.iter_mut().enumerate() {
            *b = i as u8;
        }
        let w = Wrapper { data };
        let json = serde_json::to_string(&w).unwrap();
        let back: Wrapper = serde_json::from_str(&json).unwrap();
        assert_eq!(back, w);
    }

    #[test]
    fn rejects_oversized_save_area_without_unbounded_growth() {
        // A malformed snapshot with a 109th element must be rejected. The
        // tuple visitor stops after the fixed 108-element window rather than
        // deserializing an unbounded buffer first (MDD-BUG-CRUCIBLE-00029).
        let oversized = format!("{{\"data\":[{}]}}", vec!["1"; 109].join(","));
        let err = serde_json::from_str::<Wrapper>(&oversized).unwrap_err();
        assert!(
            err.to_string().contains("length"),
            "expected a length error, got: {err}"
        );
    }

    #[test]
    fn rejects_oversized_save_area_at_109th_element_without_vec_path() {
        let next_element_calls = Cell::new(0);

        let err = super::array_108::deserialize(OversizedTupleProbe {
            next_element_calls: &next_element_calls,
        })
        .unwrap_err();

        assert_eq!(
            next_element_calls.get(),
            109,
            "fixed visitor should inspect exactly the first extra element"
        );
        assert!(
            err.to_string().contains("length"),
            "expected a length error, got: {err}"
        );
    }

    #[test]
    fn rejects_undersized_save_area() {
        let undersized = format!("{{\"data\":[{}]}}", vec!["1"; 107].join(","));
        let err = serde_json::from_str::<Wrapper>(&undersized).unwrap_err();
        assert!(
            err.to_string().contains("length"),
            "expected a length error, got: {err}"
        );
    }
}
