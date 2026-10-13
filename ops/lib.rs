// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![doc = include_str!("README.md")]

use proc_macro::TokenStream;
use std::error::Error;

mod op2;
mod webidl;

/// A macro designed to provide an extremely fast V8->Rust interface layer.
#[doc = include_str!("op2/README.md")]
#[proc_macro_attribute]
pub fn op2(attr: TokenStream, item: TokenStream) -> TokenStream {
  op2_macro(attr, item)
}

/// Derives `deno_core::webidl::WebIdlConverter` for a dictionary or an enum,
/// following the WebIDL JS-to-IDL algorithms. Re-exported as
/// `deno_core::WebIDL`.
///
/// Dictionaries are structs with named fields, marked `#[webidl(dictionary)]`.
/// Each field is a member, read from the JS key that is the field name in
/// camelCase (`foo_bar` becomes `fooBar`). Members are read in lexicographic
/// order of their keys.
///
/// Enums are marked `#[webidl(enum)]` and must only have unit variants. The
/// JS value is converted to a string, and must equal the kebab-case name of a
/// variant (`FooBar` becomes `"foo-bar"`).
///
/// Attributes:
///
/// - `#[webidl(rename = "key")]` on a member or variant uses `key` verbatim.
/// - `#[webidl(default = expr)]` on a member uses `expr` when the value is
///   `undefined`.
/// - `#[webidl(required)]` on a member makes `undefined` an error. It can not
///   be combined with `default`.
/// - `#[options(name = value, ...)]` on a member sets fields of the member
///   type's `Options` (for example `#[options(enforce_range = true)]`).
///
/// A member of type `Option<T>` is `None` when it is missing, `undefined` or
/// `null`. Any other member that is missing is an error, unless it has a
/// `default`.
///
/// ```ignore
/// #[derive(deno_core::WebIDL)]
/// #[webidl(dictionary)]
/// struct Options {
///   #[webidl(default = String::from("GET"))]
///   http_method: String,
///   #[options(enforce_range = true)]
///   retries: u8,
///   body: Option<String>,
/// }
///
/// #[derive(deno_core::WebIDL)]
/// #[webidl(enum)]
/// enum Mode {
///   FooBar,
///   #[webidl(rename = "no-cors")]
///   NoCors,
/// }
/// ```
#[proc_macro_derive(WebIDL, attributes(webidl, options))]
pub fn derive_webidl(input: TokenStream) -> TokenStream {
  let input = syn::parse_macro_input!(input as syn::DeriveInput);
  match webidl::derive(input) {
    Ok(output) => output.into(),
    Err(err) => err.to_compile_error().into(),
  }
}

fn op2_macro(attr: TokenStream, item: TokenStream) -> TokenStream {
  match crate::op2::op2(attr.into(), item.into()) {
    Ok(output) => output.into(),
    Err(err) => {
      let mut err: &dyn Error = &err;
      let mut output = "Failed to parse #[op2]:\n".to_owned();
      loop {
        output += &format!(" - {err}\n");
        if let Some(source) = err.source() {
          err = source;
        } else {
          break;
        }
      }
      panic!("{output}");
    }
  }
}
