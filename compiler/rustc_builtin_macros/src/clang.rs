use rustc_ast::tokenstream::TokenStream;
use rustc_ast::{
    self as ast, ClangImport, DUMMY_NODE_ID, ForeignMod, ItemKind, Safety, token,
};
use rustc_errors::PResult;
use rustc_expand::base::{DummyResult, ExpandResult, ExtCtxt, MacEager, MacroExpanderResult};
use rustc_parse::exp;
use rustc_parse::parser::ForceCollect;
use rustc_span::Span;
use smallvec::smallvec;
use thin_vec::ThinVec;

/// ```ignore (illustrative)
/// clang! {
///     source: "add.cpp";
///     fn cpp_add(a: i32, b: i32) -> i32;
/// }
/// ```
///
/// Expands directly into an `ItemKind::ClangImport` AST node (no `extern`
/// keyword anywhere). The contained declarations are handled like the items of
/// an `extern` block, and `source` is recorded for the codegen backend, which
/// compiles it via clang and links the resulting bitcode into the crate.
///
/// The declared functions are plain foreign items: calling them is `unsafe`, and
/// matching the C/C++ signature is the caller's responsibility.
pub(crate) fn expand_clang<'cx>(
    ecx: &'cx mut ExtCtxt<'_>,
    sp: Span,
    tts: TokenStream,
) -> MacroExpanderResult<'cx> {
    ExpandResult::Ready(match parse_clang(ecx, sp, tts) {
        Ok(item) => MacEager::items(smallvec![item]),
        Err(err) => {
            let guar = err.emit();
            DummyResult::any(sp, guar)
        }
    })
}

fn parse_clang<'a>(
    ecx: &ExtCtxt<'a>,
    sp: Span,
    tts: TokenStream,
) -> PResult<'a, Box<ast::Item>> {
    let mut p = ecx.new_parser_from_tts(tts);

    // `source : "path" ;`
    let kw = p.parse_ident()?;
    if kw.name.as_str() != "source" {
        return Err(p.dcx().struct_span_err(kw.span, "expected `source`"));
    }
    p.expect(exp!(Colon))?;
    let source = match p.parse_str_lit() {
        Ok(lit) => lit.symbol_unescaped,
        Err(_) => {
            return Err(p
                .dcx()
                .struct_span_err(p.token.span, "expected a string literal path after `source:`"));
        }
    };
    p.expect(exp!(Semi))?;

    // The remaining tokens are foreign declarations, exactly like the body of an
    // `extern` block.
    let mut items = ThinVec::new();
    while p.token.kind != token::Eof {
        match p.parse_foreign_item(ForceCollect::No)? {
            Some(Some(item)) => items.push(item),
            // A stray `;` or a recovered-but-empty item: nothing to add.
            Some(None) => {}
            None => break,
        }
    }

    let fmod = ForeignMod { extern_span: sp, safety: Safety::Default, abi: None, items };
    Ok(Box::new(ast::Item {
        attrs: ast::AttrVec::new(),
        id: DUMMY_NODE_ID,
        kind: ItemKind::ClangImport(Box::new(ClangImport { source, fmod })),
        vis: ast::Visibility {
            span: sp.shrink_to_lo(),
            kind: ast::VisibilityKind::Inherited,
            tokens: None,
        },
        span: sp,
        tokens: None,
    }))
}
