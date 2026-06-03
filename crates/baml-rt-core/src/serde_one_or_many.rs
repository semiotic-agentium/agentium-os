// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Deserialize JSON that may be **one `T` or a `Vec<T>`** into `Vec<T>` / `Option<Vec<T>>`.
//!
//! BAML unions such as `Block | Block[]` often deserialize from the LLM as a **single object**;
//! Rust DTOs typically use `Vec<T>` or `Option<Vec<T>>`, which serde maps from JSON **arrays only**.
//! Use [`deserialize_optional_vec_or_one`] or [`deserialize_vec_or_one`] with `#[serde(deserialize_with = "...")]`
//! so wire shapes stay compatible. Pair with `#[baml(vec_or_one)]` on the same field for JSON Schema / TS.
//!
//! Boilerplate shims (turbofish cannot appear in `deserialize_with` string paths):
//! [`define_optional_vec_or_one_shim!`], [`define_vec_or_one_shim!`].

use serde::{Deserialize, Deserializer};

#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany<T> {
    One(T),
    Many(Vec<T>),
}

/// `null`, missing (with `#[serde(default, deserialize_with = "...")]`), array, or single object → `Option<Vec<T>>`.
pub fn deserialize_optional_vec_or_one<'de, T, D>(
    deserializer: D,
) -> Result<Option<Vec<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    let opt = Option::<OneOrMany<T>>::deserialize(deserializer)?;
    Ok(opt.map(|v| match v {
        OneOrMany::One(b) => vec![b],
        OneOrMany::Many(v) => v,
    }))
}

/// Array or single object → `Vec<T>`. `null` is rejected by serde for [`OneOrMany`].
pub fn deserialize_vec_or_one<'de, T, D>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    let v = OneOrMany::<T>::deserialize(deserializer)?;
    Ok(match v {
        OneOrMany::One(b) => vec![b],
        OneOrMany::Many(v) => v,
    })
}

/// Defines `fn $name<'de, D>(D) -> Result<Option<Vec<$T>>, D::Error>` for use with `deserialize_with`.
#[macro_export]
macro_rules! define_optional_vec_or_one_shim {
    ($fn_name:ident, $T:ty) => {
        fn $fn_name<'de, D>(
            deserializer: D,
        ) -> ::std::result::Result<::std::option::Option<::std::vec::Vec<$T>>, D::Error>
        where
            D: ::serde::Deserializer<'de>,
        {
            $crate::serde_one_or_many::deserialize_optional_vec_or_one(deserializer)
        }
    };
}

/// Defines `fn $name<'de, D>(D) -> Result<Vec<$T>, D::Error>` for use with `deserialize_with`.
#[macro_export]
macro_rules! define_vec_or_one_shim {
    ($fn_name:ident, $T:ty) => {
        fn $fn_name<'de, D>(deserializer: D) -> ::std::result::Result<::std::vec::Vec<$T>, D::Error>
        where
            D: ::serde::Deserializer<'de>,
        {
            $crate::serde_one_or_many::deserialize_vec_or_one(deserializer)
        }
    };
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use serde_json::json;

    use super::*;

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct Block {
        k: u32,
    }

    #[derive(Debug, Deserialize)]
    struct OptField {
        #[serde(default, deserialize_with = "deserialize_optional_vec_or_one")]
        items: Option<Vec<Block>>,
    }

    #[derive(Debug, Deserialize)]
    struct ReqField {
        #[serde(deserialize_with = "deserialize_vec_or_one")]
        items: Vec<Block>,
    }

    #[test]
    fn deserialize_vec_or_one_matrix() {
        let v: OptField = serde_json::from_value(json!({})).expect("optional_missing");
        assert!(v.items.is_none());

        let v: OptField = serde_json::from_value(json!({ "items": null })).expect("optional_null");
        assert!(v.items.is_none());

        let v: OptField = serde_json::from_value(json!({ "items": [] })).expect("optional_empty");
        assert_eq!(v.items, Some(vec![]));

        let v: OptField = serde_json::from_value(json!({ "items": [{ "k": 1 }, { "k": 2 }] }))
            .expect("optional_array");
        assert_eq!(v.items, Some(vec![Block { k: 1 }, Block { k: 2 }]));

        let v: OptField =
            serde_json::from_value(json!({ "items": { "k": 7 } })).expect("optional_singleton");
        assert_eq!(v.items, Some(vec![Block { k: 7 }]));

        let v: ReqField =
            serde_json::from_value(json!({ "items": { "k": 3 } })).expect("required_singleton");
        assert_eq!(v.items, vec![Block { k: 3 }]);

        let v: ReqField =
            serde_json::from_value(json!({ "items": [{ "k": 0 }] })).expect("required_array");
        assert_eq!(v.items, vec![Block { k: 0 }]);

        let err = serde_json::from_value::<ReqField>(json!({ "items": null })).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("invalid type") || msg.contains("data did not match"),
            "required_null: unexpected error: {err}"
        );
    }
}
