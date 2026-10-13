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

/// Implements `deno_core::webidl::WebIdlConverter` for a WebIDL dictionary
/// (`#[webidl(dictionary)]` on a struct with named fields) or enum
/// (`#[webidl(enum)]` on an enum of unit variants).
///
/// Dictionary members are keyed by the field name in camelCase unless
/// renamed with `#[webidl(rename = "key")]`. A member that is missing or
/// undefined takes its `#[webidl(default = <expr>)]`, is `None` for an
/// `Option<T>` field, and is otherwise an error. `#[webidl(required)]` makes
/// a member required and `#[options(<option> = <value>, ...)]` sets the
/// member's converter options.
///
/// Enum variants match their name in kebab-case unless renamed with
/// `#[webidl(rename = "value")]`.
#[proc_macro_derive(WebIDL, attributes(webidl, options))]
pub fn webidl(item: TokenStream) -> TokenStream {
  match webidl::webidl(item.into()) {
    Ok(output) => output.into(),
    Err(err) => err.into_compile_error().into(),
  }
}
