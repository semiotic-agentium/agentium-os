// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

pub fn escape_baml_description(raw: &str) -> String {
    raw.replace('\\', "\\\\").replace('"', "\\\"")
}
