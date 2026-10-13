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

/// Derives `deno_core::webidl::WebIdlConverter` for a dictionary or an enum, so
/// that the type can be passed to an op with `#[webidl]`. Re-exported from
/// `deno_core` as `deno_core::WebIDL`.
///
/// Dictionaries are structs marked `#[webidl(dictionary)]`. Each field is a
/// member whose key is the field name in lowerCamelCase, or the value of
/// `#[webidl(rename = "key")]`. Members are read in the order of their keys.
/// Attributes on a field:
///
/// - `#[webidl(default = expr)]`: used when the member is missing or `undefined`.
/// - `#[webidl(required)]`: the member must be present. Missing is an error.
/// - `#[options(field = value, ...)]`: sets fields of the member's `Options`.
///
/// A missing member with no default becomes `None` for an `Option<T>` field
/// and is otherwise an error.
///
/// Enums are unit-only enums marked `#[webidl(enum)]`. The JavaScript value is
/// converted to a string and must exactly match a variant's kebab-case name,
/// or the value of `#[webidl(rename = "value")]`.
///
/// Generic types are not supported.
#[proc_macro_derive(WebIDL, attributes(webidl, options))]
pub fn derive_webidl(input: TokenStream) -> TokenStream {
  match webidl::webidl(input.into()) {
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
