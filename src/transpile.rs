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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_return() {
        let source = "async function __agent__() {\nreturn tools\n}";
        let result = ts_to_js(source);
        assert!(result.is_ok(), "failed: {:?}", result);
        let js = result.unwrap();
        assert!(js.contains("return tools"), "output: {js}");
    }

    #[test]
    fn test_with_type_declarations() {
        let source = r#"
declare const tools: Array<{ server: string; name: string; description: string; input_schema: any }>;

declare const chrome_devtools: {
  /** Take a screenshot */
  take_screenshot(params: { url: string }): Promise<any>;
};

async function __agent__() {
return tools.filter(t => t.name.includes("screenshot"))
}
"#;
        let result = ts_to_js(source);
        assert!(result.is_ok(), "failed: {:?}", result);
        let js = result.unwrap();
        assert!(js.contains("return tools.filter"), "output: {js}");
        // Type declarations should be stripped
        assert!(!js.contains("declare"), "declarations not stripped: {js}");
    }

    #[test]
    fn test_arrow_function() {
        let source = "async function __agent__() {\nconst result = tools.map(t => ({ server: t.server, name: t.name }));\nreturn result;\n}";
        let result = ts_to_js(source);
        assert!(result.is_ok(), "failed: {:?}", result);
    }
}
