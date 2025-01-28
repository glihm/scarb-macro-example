extern crate cairo_lang_macro;
extern crate cairo_lang_parser;
extern crate cairo_lang_syntax;
extern crate cairo_lang_utils;
extern crate cairo_lang_defs;
extern crate cairo_lang_diagnostics;

use cairo_lang_macro::{attribute_macro, quote, Diagnostic, Diagnostics, ProcMacroResult, Severity, TextSpan, Token, TokenStream, TokenTree};
use cairo_lang_parser::utils::SimpleParserDatabase;
use cairo_lang_syntax::node::ast::MaybeModuleBody;
use cairo_lang_syntax::node::helpers::BodyItems;
use cairo_lang_syntax::node::{Terminal, TypedSyntaxNode};
use cairo_lang_syntax::node::{ast, with_db::SyntaxNodeWithDb};
use cairo_lang_syntax::node::kind::SyntaxKind::ItemModule;
use cairo_lang_defs::patcher::{PatchBuilder, RewriteNode};
use cairo_lang_utils::ordered_hash_map::OrderedHashMap;
use cairo_lang_utils::unordered_hash_map::UnorderedHashMap;

fn debug_expand(loc: &str, code: &str) {
  if std::env::var("DOJO_EXPAND").is_ok() {
      println!("\n// *> EXPAND {} <*\n{}\n\n", loc, code);
  }
}

/*
#[attribute_macro]
pub fn some(_args: TokenStream, _token_stream: TokenStream) -> ProcMacroResult {
    let db_val = SimpleParserDatabase::default();
    let db = &db_val;
    let code = r#"
              #[derive(Drop)]
              struct Rectangle {
                  width: u64,
                  height: u64,
              }
              #[derive(Drop, PartialEq)]
              struct Square {
                  side_length: u64,
              }
              impl RectangleIntoSquare of TryInto<Rectangle, Square> {
                  fn try_into(self: Rectangle) -> Option<Square> {
                      if self.height == self.width {
                          Option::Some(Square { side_length: self.height })
                      } else {
                          Option::None
                      }
                  }
              }
              fn main() {
                let rectangle = Rectangle { width: 8, height: 8 };
                let result: Square = rectangle.try_into().unwrap();
                let expected = Square { side_length: 8 };
                assert!(
                    result == expected,
                    "Rectangle with equal width and height should be convertible to a square."
                );
                let rectangle = Rectangle { width: 5, height: 8 };
                let result: Option<Square> = rectangle.try_into();
                assert!(
                    result.is_none(),
                    "Rectangle with different width and height should not be convertible to a square."
                );
              }
          "#;
    let syntax_node = db.parse_virtual(code).unwrap();
    let syntax_node_with_db = SyntaxNodeWithDb::new(&syntax_node, db);
    let tokens = quote! {
      #syntax_node_with_db
      trait Circle {
        fn print() -> ();
      }
      impl CircleImpl of Circle {
        fn print() -> () {
          println!("This is a circle!");
        }
      }
    };
    ProcMacroResult::new(tokens)
}
*/

#[attribute_macro]
pub fn some(_args: TokenStream, token_stream: TokenStream) -> ProcMacroResult {
  let db = SimpleParserDatabase::default();
  let (root_node, _diagnostics) = db.parse_virtual_with_diagnostics(token_stream);

  for n in root_node.descendants(&db) {
    // Process only the first module expected to be the contract.
    if n.kind(&db) == ItemModule {
        let module_ast = ast::ItemModule::from_syntax_node(&db, n);
        return from_module(&db, &module_ast);
    }
}

  ProcMacroResult::new(TokenStream::empty())
}

pub fn from_module(db: &SimpleParserDatabase, module_ast: &ast::ItemModule) -> ProcMacroResult {
  const CONSTRUCTOR_FN: &str = "constructor";
  const CONTRACT_PATCH: &str = include_str!("./contract.patch.cairo");

  let name = module_ast.name(db).text(db);

  let mut diagnostics = vec![];

  let mut has_storage = false;
  let mut has_constructor = false;

  if let MaybeModuleBody::Some(body) = module_ast.body(db) {
      // TODO: Use `.iter_items_in_cfg(db, metadata.cfg_set)` when possible
      // to ensure we don't loop on items that are not in the current cfg set.
      let mut body_nodes: Vec<_> = body
          .items_vec(db)
          .iter()
          .flat_map(|el| {
              if let ast::ModuleItem::Enum(ref enum_ast) = el {
                  if enum_ast.name(db).text(db).to_string() == "Event" {
                      diagnostics.push(Diagnostic::error(
                        "Event is not supported",
                      ));
                  }
              } else if let ast::ModuleItem::Struct(ref struct_ast) = el {
                  if struct_ast.name(db).text(db).to_string() == "Storage" {
                      has_storage = true;
                  }
              } else if let ast::ModuleItem::FreeFunction(ref fn_ast) = el {
                  let fn_decl = fn_ast.declaration(db);
                  let fn_name = fn_decl.name(db).text(db);

                  if fn_name == CONSTRUCTOR_FN {
                      has_constructor = true;
                  }
              }

              vec![RewriteNode::Copied(el.as_syntax_node())]
          })
          .collect();

      if !has_constructor {
          let node = RewriteNode::Text(
              "
                  #[constructor]
                      fn constructor(ref self: ContractState) {
                          // expected error here since self.a is not defined.
                          // Uncomment to see how error is reported coming from macro.
                          // self.a = 1;
                      }
                  "
              .to_string(),
          );

          body_nodes.append(&mut vec![node]);
      }

      let mut builder = PatchBuilder::new(db, module_ast);
      builder.add_modified(RewriteNode::Mapped {
          node: Box::new(RewriteNode::interpolate_patched(
              CONTRACT_PATCH,
              &UnorderedHashMap::from([
                  ("name".to_string(), RewriteNode::Text(name.to_string())),
                  ("body".to_string(), RewriteNode::new_modified(body_nodes)),
              ]),
          )),
          origin: module_ast.as_syntax_node().span_without_trivia(db),
      });

      // Code mappings not used?
      let (code, _) = builder.build();
      debug_expand(&format!("CONTRACT PATCH: {name}"), &code);

      // 1. Using this approach, doesn't seem that the diags are actually mapped out correctly.
      let token_stream = TokenStream::new(vec![TokenTree::Ident(Token::new(code.to_string(), TextSpan::call_site()))]);
      // Comment this to test the second approach.
      return ProcMacroResult::new(token_stream);

      // There is also a parse virtual with diagnostics function, to be checked.
      // Seems we are only one line off (one line below the actual error).
      let (syntax_node, diagnostics) = db.parse_virtual_with_diagnostics(code);
      let syntax_node_with_db = SyntaxNodeWithDb::new(&syntax_node, db);

      let tokens = quote! {
        #syntax_node_with_db
      };

      let diags = diagnostics.format_with_severity(db, &OrderedHashMap::default());

      return ProcMacroResult::new(tokens).with_diagnostics(Diagnostics::new(diags.into_iter().map(|d| Diagnostic {
        message: d.message().to_string(),
        severity: if d.severity() == cairo_lang_diagnostics::Severity::Error { Severity::Error } else { Severity::Warning },
      }).collect()));
  }

  ProcMacroResult::new(TokenStream::empty())
}
