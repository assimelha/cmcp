use std::path::Path;

use oxc::allocator::Allocator;
use oxc::codegen::Codegen;
use oxc::parser::Parser;
use oxc::semantic::SemanticBuilder;
use oxc::span::SourceType;
use oxc::transformer::{TransformOptions, Transformer};

/// Transpile TypeScript to JavaScript by stripping type annotations.
pub fn ts_to_js(source: &str) -> Result<String, String> {
    let allocator = Allocator::default();
    let path = Path::new("input.ts");
    let source_type = SourceType::from_path(path).map_err(|e| format!("{e}"))?;

    // Parse
    let parser_ret = Parser::new(&allocator, source, source_type).parse();
    if !parser_ret.errors.is_empty() {
        let msgs: Vec<String> = parser_ret.errors.iter().map(|e| format!("{e}")).collect();
        return Err(format!("parse error: {}", msgs.join("; ")));
    }
    let mut program = parser_ret.program;

    // Semantic analysis (required by transformer)
    let semantic_ret = SemanticBuilder::new()
        .with_excess_capacity(2.0)
        .build(&program);
    if !semantic_ret.errors.is_empty() {
        let msgs: Vec<String> = semantic_ret.errors.iter().map(|e| format!("{e}")).collect();
        return Err(format!("semantic error: {}", msgs.join("; ")));
    }
    let scoping = semantic_ret.semantic.into_scoping();

    // Transform (strip types)
    let options = TransformOptions::default();
    let transform_ret = Transformer::new(&allocator, path, &options)
        .build_with_scoping(scoping, &mut program);
    if !transform_ret.errors.is_empty() {
        let msgs: Vec<String> = transform_ret.errors.iter().map(|e| format!("{e}")).collect();
        return Err(format!("transform error: {}", msgs.join("; ")));
    }

    // Codegen
    let js = Codegen::new().build(&program).code;
    Ok(js)
}
