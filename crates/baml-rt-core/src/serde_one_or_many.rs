//! Deserialize JSON that may be **one `T` or a `Vec<T>`** into `Vec<T>` / `Option<Vec<T>>`.
//!
//! BAML unions such as `Block | Block[]` often deserialize from the LLM as a **single object**;
//! Rust DTOs typically use `Vec<T>` or `Option<Vec<T>>`, which serde maps from JSON **arrays only**.
//! Use [`deserialize_optional_vec_or_one`] or [`deserialize_vec_or_one`] with `#[serde(deserialize_with = "...")]`
//! so wire shapes stay compatible.

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
    fn optional_missing_is_none() {
        let v: OptField = serde_json::from_value(json!({})).expect("deserialize");
        assert!(v.items.is_none());
    }

    #[test]
    fn optional_null_is_none() {
        let v: OptField = serde_json::from_value(json!({ "items": null })).expect("deserialize");
        assert!(v.items.is_none());
    }

    #[test]
    fn optional_empty_array() {
        let v: OptField = serde_json::from_value(json!({ "items": [] })).expect("deserialize");
        assert_eq!(v.items, Some(vec![]));
    }

    #[test]
    fn optional_array_of_two() {
        let v: OptField = serde_json::from_value(json!({ "items": [{ "k": 1 }, { "k": 2 }] }))
            .expect("deserialize");
        assert_eq!(v.items, Some(vec![Block { k: 1 }, Block { k: 2 }]));
    }

    #[test]
    fn optional_single_object_becomes_one_element_vec() {
        let v: OptField =
            serde_json::from_value(json!({ "items": { "k": 7 } })).expect("deserialize");
        assert_eq!(v.items, Some(vec![Block { k: 7 }]));
    }

    #[test]
    fn required_single_object() {
        let v: ReqField =
            serde_json::from_value(json!({ "items": { "k": 3 } })).expect("deserialize");
        assert_eq!(v.items, vec![Block { k: 3 }]);
    }

    #[test]
    fn required_array() {
        let v: ReqField =
            serde_json::from_value(json!({ "items": [{ "k": 0 }] })).expect("deserialize");
        assert_eq!(v.items, vec![Block { k: 0 }]);
    }

    #[test]
    fn required_null_errors() {
        let err = serde_json::from_value::<ReqField>(json!({ "items": null })).unwrap_err();
        assert!(
            err.to_string().contains("invalid type")
                || err.to_string().contains("data did not match"),
            "unexpected error: {err}"
        );
    }
}
