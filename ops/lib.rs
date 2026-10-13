// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![doc = include_str!("README.md")]

use proc_macro::TokenStream;
use std::error::Error;

mod op2;
mod webidl;

/// Derives `deno_core::webidl::WebIdlConverter` for a dictionary
/// (`#[webidl(dictionary)]` on a struct with named fields) or an enum of unit
/// variants (`#[webidl(enum)]`).
#[proc_macro_derive(WebIDL, attributes(webidl, options))]
pub fn webidl_derive(input: TokenStream) -> TokenStream {
  match webidl::expand(input.into()) {
    Ok(output) => output.into(),
    Err(err) => err.to_compile_error().into(),
  }
}

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
