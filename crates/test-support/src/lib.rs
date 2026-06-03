// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Shared test support utilities for the BAML runtime workspace.

#![recursion_limit = "256"]

pub mod common;
pub mod incident_fixtures;
pub mod support;
pub mod testing;

/// Async integration test: isolated store → setup → query → `insta` JSON snapshot.
///
/// `$snap_name` is the snapshot label (e.g. `"failed_calls@surreal"`).
#[macro_export]
macro_rules! json_snapshot_test {
    ($test_name:ident, $snap_name:expr, $setup:expr, $query:expr) => {
        ::paste::paste! {
            #[::tokio::test]
            async fn [<$test_name>]() {
                let store = $crate::testing::provenance_fixtures::build_isolated_store().await;
                $setup(&*store).await;
                let result = $query(&*store).await;
                ::insta::assert_json_snapshot!($snap_name, result);
            }
        }
    };
}
