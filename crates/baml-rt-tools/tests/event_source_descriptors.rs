// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Integration: linked tool crates register event source type descriptors.

use baml_rt_tools::all_event_source_type_descriptors;
#[allow(unused_imports)]
use baml_tools_clickup as _;
#[allow(unused_imports)]
use baml_tools_github as _;
#[allow(unused_imports)]
use baml_tools_slack as _;
#[allow(unused_imports)]
use baml_tools_system as _;

#[test]
fn linked_tools_register_source_record_descriptors() {
    let ids: Vec<&str> = all_event_source_type_descriptors()
        .into_iter()
        .map(|d| d.descriptor_id)
        .collect();

    for expected in [
        "slack-source-records",
        "clickup-source-records",
        "github-issues-source-records",
        "system-callback-token",
    ] {
        assert!(
            ids.contains(&expected),
            "missing descriptor {expected}; got {ids:?}"
        );
    }
}
