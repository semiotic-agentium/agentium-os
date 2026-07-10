// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use baml_rt_provenance::HOST_COMPACTION_BAML_FUNCTION;
use baml_rt_quickjs::BamlRuntimeManager;

const HOST_CLIENTS_BAML: &str = include_str!("../../../baml_src/host/clients.baml");
const HOST_COMPACTION_BAML: &str = include_str!("../../../baml_src/host/context_compaction.baml");

const EMBEDDED_HOST_BAML: &[(&str, &str)] = &[
    ("baml_src/host/clients.baml", HOST_CLIENTS_BAML),
    (
        "baml_src/host/context_compaction.baml",
        HOST_COMPACTION_BAML,
    ),
];

#[test]
fn load_schema_from_files_registers_embedded_host_compaction_function() {
    let mut manager = BamlRuntimeManager::new().expect("manager");
    manager
        .load_schema_from_files("baml_src", EMBEDDED_HOST_BAML)
        .expect("load embedded host BAML");

    assert!(
        manager
            .get_function_signature(HOST_COMPACTION_BAML_FUNCTION)
            .is_some()
    );
}

#[test]
fn load_schema_from_files_does_not_require_disk_root() {
    let files = &[
        ("/nonexistent/baml_src/host/clients.baml", HOST_CLIENTS_BAML),
        (
            "/nonexistent/baml_src/host/context_compaction.baml",
            HOST_COMPACTION_BAML,
        ),
    ];

    let mut manager = BamlRuntimeManager::new().expect("manager");
    manager
        .load_schema_from_files("/nonexistent/baml_src", files)
        .expect("load embedded host BAML from nonexistent root");

    assert!(
        manager
            .get_function_signature(HOST_COMPACTION_BAML_FUNCTION)
            .is_some()
    );
}
