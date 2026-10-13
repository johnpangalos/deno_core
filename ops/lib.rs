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
/// (`#[webidl(dictionary)]` on a struct with named fields) or enumeration
/// (`#[webidl(enum)]` on an enum of unit variants).
///
/// Dictionary members are keyed by the field name in camelCase, and enum
/// values are the variant name in kebab-case; either can be changed with
/// `#[webidl(rename = "...")]`. Dictionary members also accept
/// `#[webidl(default = <expr>)]`, `#[webidl(required)]` and
/// `#[options(<option> = <value>, ...)]`, which sets the member converter's
/// options (for example `#[options(clamp = true)]`).
///
/// ```ignore
/// #[derive(WebIDL)]
/// #[webidl(dictionary)]
/// pub struct RequestInit {
///   #[webidl(default = String::from("GET"))]
///   method: String,
///   #[webidl(required)]
///   body: Option<String>,
///   #[options(enforce_range = true)]
///   priority: Option<u8>,
///   mode: RequestMode,
/// }
///
/// #[derive(WebIDL)]
/// #[webidl(enum)]
/// pub enum RequestMode {
///   Cors,
///   NoCors,
///   #[webidl(rename = "same-origin")]
///   SameOrigin,
/// }
/// ```
#[proc_macro_derive(WebIDL, attributes(webidl, options))]
pub fn webidl(item: TokenStream) -> TokenStream {
  match webidl::webidl(item.into()) {
    Ok(output) => output.into(),
    Err(err) => err.into_compile_error().into(),
  }
}
