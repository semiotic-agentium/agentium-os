// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { marked } from "marked";
import DOMPurify from "dompurify";

marked.setOptions({
  breaks: true,
  gfm: true,
});

const PURIFY_CONFIG = {
  ALLOWED_TAGS: [
    "p",
    "br",
    "strong",
    "em",
    "s",
    "code",
    "pre",
    "blockquote",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "ul",
    "ol",
    "li",
    "table",
    "thead",
    "tbody",
    "tr",
    "th",
    "td",
    "a",
    "hr",
    "img",
  ],
  ALLOWED_ATTR: ["href", "target", "rel", "src", "alt", "class"],
};

export function renderMarkdown(text: string | null | undefined): string {
  if (!text) return "";
  const raw = marked(text) as string;
  return DOMPurify.sanitize(raw, PURIFY_CONFIG) as string;
}
