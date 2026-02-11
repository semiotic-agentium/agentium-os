use crate::tools::ToolFunctionMetadata;
use baml_rt_core::{BamlRtError, Result};
use genco::lang::js;
use genco::prelude::*;

pub fn render_tool_typescript(tools: &[ToolFunctionMetadata]) -> Result<String> {
    let mut tokens: js::Tokens = quote!(
        // TypeScript declarations for host tools
        // This file is auto-generated - do not edit manually
    );
    tokens.line();

    quote_in!(tokens =>
        export type ToolFailureKind =
            | "InvalidInput"
            | "ExecutionFailed"
            | "NotAuthorized"
            | "RateLimited"
            | "Cancelled"
            | "Unknown";

        export interface ToolFailure {
            kind: ToolFailureKind;
            message: string;
            retryable: boolean;
        }

        export type ToolStep<O> =
            | { status: "streaming"; output: O }
            | { status: "done"; output?: O }
            | { status: "error"; error: ToolFailure };

        export interface ToolSession<I, O> {
            sessionId: string;
            send(input: I): Promise<void>;
            continue(): Promise<ToolStep<O>>;
            finish(): Promise<void>;
            abort(reason?: string): Promise<void>;
        }
    );
    tokens.line();

    let tool_name_union = if tools.is_empty() {
        "never".to_string()
    } else {
        tools
            .iter()
            .map(|tool| format!("\"{}\"", tool.name))
            .collect::<Vec<_>>()
            .join(" | ")
    };

    quote_in!(tokens => export type ToolName = $(tool_name_union););
    tokens.line();

    for tool in tools {
        for extra in &tool.extra_ts_decls {
            for line in extra.lines() {
                quote_in!(tokens => $(line));
            }
            tokens.line();
        }
        if let Some(ts) = &tool.input_type.ts_decl {
            for line in ts.lines() {
                quote_in!(tokens => $(line));
            }
            tokens.line();
        }
        if let Some(ts) = &tool.output_type.ts_decl {
            for line in ts.lines() {
                quote_in!(tokens => $(line));
            }
            tokens.line();
        }
    }

    if !tools.is_empty() {
        let mut input_map: js::Tokens = quote!();
        for tool in tools {
            let line = format!("\"{}\": {};", tool.name, tool.input_type.name);
            quote_in!(input_map => $(line));
            input_map.push();
        }
        let mut output_map: js::Tokens = quote!();
        for tool in tools {
            let line = format!("\"{}\": {};", tool.name, tool.output_type.name);
            quote_in!(output_map => $(line));
            output_map.push();
        }

        quote_in!(tokens => export interface ToolInputMap { $(input_map) });
        quote_in!(tokens => export interface ToolOutputMap { $(output_map) });

        quote_in!(tokens =>
            export type ToolInput<T extends ToolName> = ToolInputMap[T];
        );
        quote_in!(tokens =>
            export type ToolOutput<T extends ToolName> = ToolOutputMap[T];
        );

        quote_in!(tokens =>
            declare function openToolSession<T extends ToolName>(toolName: T): Promise<ToolSession<ToolInput<T>, ToolOutput<T>>>;
        );
        tokens.line();
    }

    for tool in tools {
        // Use the type-safe class_name derived from Bundle + Tool types
        let fn_name = format!("open{}Session", tool.class_name);
        let tool_literal = format!("\"{}\"", tool.name);
        let tool_literal_ref = tool_literal.as_str();
        quote_in!(tokens =>
            declare function $(fn_name)(): Promise<ToolSession<ToolInput<$(tool_literal_ref)>, ToolOutput<$(tool_literal_ref)>>>;
        );
        tokens.push();
    }

    tokens
        .to_file_string()
        .map_err(|e| BamlRtError::InvalidArgument(format!("TypeScript render error: {}", e)))
    // Note: genco::Error doesn't implement std::error::Error, so we preserve message context
}

// Deprecated: Use tool.class_name instead, which is derived type-safely from Bundle + Tool types
#[allow(dead_code)]
pub fn tool_typescript_name(tool_name: &str) -> String {
    tool_name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<String>()
}
