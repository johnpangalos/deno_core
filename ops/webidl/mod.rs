// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

mod dictionary;
mod enum_;

use proc_macro2::TokenStream;
use quote::quote;
use syn::parse2;
use syn::spanned::Spanned;
use syn::Data;
use syn::DeriveInput;
use syn::Error;
use syn::GenericParam;
use syn::Lifetime;

pub fn webidl(item: TokenStream) -> Result<TokenStream, Error> {
  let input = parse2::<DeriveInput>(item)?;

  let mut kind = None;
  for attr in &input.attrs {
    if !attr.path().is_ident("webidl") {
      continue;
    }
    attr.parse_nested_meta(|meta| {
      if meta.path.is_ident("dictionary") {
        kind = Some(Kind::Dictionary);
      } else if meta.path.is_ident("enum") {
        kind = Some(Kind::Enum);
      } else {
        return Err(meta.error("expected `dictionary` or `enum`"));
      }
      Ok(())
    })?;
  }
  let Some(kind) = kind else {
    return Err(Error::new(
      input.ident.span(),
      "expected #[webidl(dictionary)] or #[webidl(enum)]",
    ));
  };

  // The converter's lifetime: the type's own lifetime parameter, if it has one.
  let lifetime = match input.generics.params.iter().collect::<Vec<_>>()[..] {
    [] => Lifetime::new("'a", input.ident.span()),
    [GenericParam::Lifetime(param)] => param.lifetime.clone(),
    _ => {
      return Err(Error::new(
        input.generics.span(),
        "WebIDL types may only have a single lifetime parameter",
      ))
    }
  };

  let ident = &input.ident;
  let name = ident.to_string();
  let body = match (kind, &input.data) {
    (Kind::Dictionary, Data::Struct(data)) => {
      dictionary::convert(&name, &lifetime, data)?
    }
    (Kind::Enum, Data::Enum(data)) => enum_::convert(&name, &lifetime, data)?,
    (Kind::Dictionary, _) => {
      return Err(Error::new(
        ident.span(),
        "#[webidl(dictionary)] must be used on a struct",
      ))
    }
    (Kind::Enum, _) => {
      return Err(Error::new(
        ident.span(),
        "#[webidl(enum)] must be used on an enum",
      ))
    }
  };

  let ty = if input.generics.params.is_empty() {
    quote!(#ident)
  } else {
    quote!(#ident<#lifetime>)
  };

  Ok(quote! {
    impl<#lifetime> ::deno_core::webidl::WebIdlConverter<#lifetime> for #ty {
      type Options = ();

      #[allow(clippy::field_reassign_with_default)]
      fn convert<C>(
        __scope: &mut ::deno_core::v8::HandleScope<#lifetime>,
        __value: ::deno_core::v8::Local<#lifetime, ::deno_core::v8::Value>,
        __prefix: ::std::borrow::Cow<'static, str>,
        __context: C,
        __options: &Self::Options,
      ) -> ::std::result::Result<Self, ::deno_core::webidl::WebIdlError>
      where
        C: Fn() -> ::std::borrow::Cow<'static, str>,
      {
        #body
      }
    }
  })
}

#[derive(Clone, Copy)]
enum Kind {
  Dictionary,
  Enum,
}

#[cfg(test)]
mod tests {
  use super::*;
  use quote::ToTokens;
  use std::path::PathBuf;
  use syn::File;
  use syn::Item;

  fn derives_webidl(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
      attr.path().is_ident("derive")
        && attr.to_token_stream().to_string().contains("WebIDL")
    })
  }

  fn expand_webidl(item: impl ToTokens) -> String {
    let tokens =
      webidl(item.to_token_stream()).expect("Failed to generate WebIDL");
    println!("======== Raw tokens ========:\n{}", tokens.clone());
    let tree = syn::parse2(tokens).unwrap();
    let actual = prettyplease::unparse(&tree);
    println!("======== Generated ========:\n{}", actual);
    actual
  }

  #[testing_macros::fixture("webidl/test_cases/*.rs")]
  fn test_proc_macro_sync(input: PathBuf) {
    test_proc_macro_output(input)
  }

  fn test_proc_macro_output(input: PathBuf) {
    let update_expected = std::env::var("UPDATE_EXPECTED").is_ok();

    let source =
      std::fs::read_to_string(&input).expect("Failed to read test file");

    const PRELUDE: &str = r"// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();";

    if !source.starts_with(PRELUDE) {
      panic!("Source does not start with expected prelude:]n{PRELUDE}");
    }

    let file =
      syn::parse_str::<File>(&source).expect("Failed to parse Rust file");
    let mut expected_out = vec![];
    for item in file.items {
      match item {
        Item::Struct(item) if derives_webidl(&item.attrs) => {
          expected_out.push(expand_webidl(item));
        }
        Item::Enum(item) if derives_webidl(&item.attrs) => {
          expected_out.push(expand_webidl(item));
        }
        _ => {}
      }
    }

    let expected_out = expected_out.join("\n");

    if update_expected {
      std::fs::write(input.with_extension("out"), expected_out)
        .expect("Failed to write expectation file");
    } else {
      let expected = std::fs::read_to_string(input.with_extension("out"))
        .expect("Failed to read expectation file");
      assert_eq!(
        expected, expected_out,
        "Failed to match expectation. Use UPDATE_EXPECTED=1."
      );
    }
  }

  fn expect_error(item: TokenStream, message: &str) {
    let err = webidl(item).expect_err("expected an error");
    assert_eq!(err.to_string(), message);
  }

  #[test]
  fn test_errors() {
    expect_error(
      quote!(
        struct Foo {}
      ),
      "expected #[webidl(dictionary)] or #[webidl(enum)]",
    );
    expect_error(
      quote!(
        #[webidl(union)]
        struct Foo {}
      ),
      "expected `dictionary` or `enum`",
    );
    expect_error(
      quote!(
        #[webidl(enum)]
        struct Foo {}
      ),
      "#[webidl(enum)] must be used on an enum",
    );
    expect_error(
      quote!(
        #[webidl(dictionary)]
        enum Foo {}
      ),
      "#[webidl(dictionary)] must be used on a struct",
    );
    expect_error(
      quote!(
        #[webidl(dictionary)]
        struct Foo(u8);
      ),
      "dictionaries must have named fields",
    );
    expect_error(
      quote!(
        #[webidl(dictionary)]
        struct Foo<T> {
          a: T,
        }
      ),
      "WebIDL types may only have a single lifetime parameter",
    );
    expect_error(
      quote!(
        #[webidl(dictionary)]
        struct Foo {
          #[webidl(required, default = 1)]
          a: u8,
        }
      ),
      "a member can not be both required and have a default",
    );
    expect_error(
      quote!(
        #[webidl(dictionary)]
        struct Foo {
          a: u8,
          #[webidl(rename = "a")]
          b: u8,
        }
      ),
      "duplicate dictionary member 'a'",
    );
    expect_error(
      quote!(
        #[webidl(enum)]
        enum Foo {
          A(u8),
        }
      ),
      "enum variants must not have fields",
    );
    expect_error(
      quote!(
        #[webidl(enum)]
        enum Foo {
          A,
          #[webidl(rename = "a")]
          B,
        }
      ),
      "duplicate enum value 'a'",
    );
  }
}
