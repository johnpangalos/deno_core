// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! Implementation of `#[derive(WebIDL)]`, which generates
//! `deno_core::webidl::WebIdlConverter` impls for dictionaries and enums.

use proc_macro2::TokenStream;
use quote::format_ident;
use quote::quote;

enum Kind {
  Dictionary,
  Enum,
}

struct Member {
  ident: syn::Ident,
  ty: syn::Type,
  key: String,
  default: Option<syn::Expr>,
  required: bool,
  options: Vec<(syn::Ident, syn::Expr)>,
}

pub(crate) fn webidl(input: TokenStream) -> syn::Result<TokenStream> {
  let input: syn::DeriveInput = syn::parse2(input)?;
  if !input.generics.params.is_empty() {
    return Err(syn::Error::new_spanned(
      &input.generics,
      "#[derive(WebIDL)] does not support generic parameters",
    ));
  }

  let mut kind = None;
  for attr in &input.attrs {
    if attr.path().is_ident("webidl") {
      attr.parse_nested_meta(|meta| {
        let next = if meta.path.is_ident("dictionary") {
          Kind::Dictionary
        } else if meta.path.is_ident("enum") {
          Kind::Enum
        } else {
          return Err(meta.error(
            "unsupported #[webidl] option, expected `dictionary` or `enum`",
          ));
        };
        if kind.is_some() {
          return Err(meta.error(
            "#[derive(WebIDL)] takes exactly one of `dictionary` or `enum`",
          ));
        }
        kind = Some(next);
        Ok(())
      })?;
    }
  }

  match kind {
    Some(Kind::Dictionary) => dictionary(&input),
    Some(Kind::Enum) => enumeration(&input),
    None => Err(syn::Error::new_spanned(
      &input.ident,
      "#[derive(WebIDL)] requires #[webidl(dictionary)] or #[webidl(enum)]",
    )),
  }
}

/// The header shared by generated impls. `body` is the body of `convert`.
fn impl_converter(name: &syn::Ident, body: TokenStream) -> TokenStream {
  quote! {
    impl<'a> ::deno_core::webidl::WebIdlConverter<'a> for #name {
      type Options = ();

      #[allow(unused_variables, clippy::field_reassign_with_default)]
      fn convert<C>(
        scope: &mut ::deno_core::v8::HandleScope<'a>,
        value: ::deno_core::v8::Local<'a, ::deno_core::v8::Value>,
        prefix: ::std::borrow::Cow<'static, str>,
        context: C,
        _options: &Self::Options,
      ) -> ::std::result::Result<Self, ::deno_core::webidl::WebIdlError>
      where
        C: Fn() -> ::std::borrow::Cow<'static, str>,
      {
        #body
      }
    }
  }
}

/// Returns the member key for a field: its lowerCamelCase name, without `r#`.
fn default_key(ident: &syn::Ident) -> String {
  let raw = ident.to_string();
  stringcase::camel_case(raw.strip_prefix("r#").unwrap_or(&raw))
}

fn is_option(ty: &syn::Type) -> bool {
  matches!(ty, syn::Type::Path(path) if path.path.segments.last().is_some_and(|segment| segment.ident == "Option"))
}

fn dictionary(input: &syn::DeriveInput) -> syn::Result<TokenStream> {
  let syn::Data::Struct(data) = &input.data else {
    return Err(syn::Error::new_spanned(
      &input.ident,
      "#[webidl(dictionary)] can only be used on a struct",
    ));
  };
  let syn::Fields::Named(fields) = &data.fields else {
    return Err(syn::Error::new_spanned(
      &input.ident,
      "#[webidl(dictionary)] requires a struct with named fields, tuple and unit structs are not supported",
    ));
  };
  let members = fields
    .named
    .iter()
    .map(dictionary_member)
    .collect::<syn::Result<Vec<_>>>()?;

  let mut keys = std::collections::HashSet::new();
  for member in &members {
    if !keys.insert(member.key.as_str()) {
      return Err(syn::Error::new_spanned(
        &member.ident,
        format!("duplicate dictionary key '{}'", member.key),
      ));
    }
  }

  // Members are read in the order of their JavaScript keys, not their declaration order.
  let mut order: Vec<usize> = (0..members.len()).collect();
  order.sort_by(|&a, &b| {
    members[a]
      .key
      .encode_utf16()
      .cmp(members[b].key.encode_utf16())
  });

  let steps = order.iter().map(|&index| {
    let member = &members[index];
    let var = format_ident!("__field_{}", index);
    let key = &member.key;
    let ty = &member.ty;
    let options = if member.options.is_empty() {
      quote! {
        let __options = <#ty as ::deno_core::webidl::WebIdlConverter<'a>>::Options::default();
      }
    } else {
      let sets = member
        .options
        .iter()
        .map(|(name, value)| quote!(__options.#name = #value;));
      quote! {
        #[allow(clippy::field_reassign_with_default)]
        let mut __options = <#ty as ::deno_core::webidl::WebIdlConverter<'a>>::Options::default();
        #(#sets)*
      }
    };
    let missing = if let Some(default) = &member.default {
      quote!(#default)
    } else if is_option(ty) && !member.required {
      quote!(None)
    } else {
      quote! {
        return Err(::deno_core::webidl::missing_required_key(
          &prefix,
          &dictionary_context(),
          #key,
        ))
      }
    };
    quote! {
      let #var = match ::deno_core::webidl::dictionary_member(
        scope,
        __object,
        #key,
        &prefix,
        &dictionary_context,
      )? {
        Some(__value) => {
          #options
          <#ty as ::deno_core::webidl::WebIdlConverter<'a>>::convert(
            scope,
            __value,
            prefix.clone(),
            || ::deno_core::webidl::nested_context(#key, &dictionary_context()),
            &__options,
          )?
        }
        None => #missing,
      };
    }
  });

  let fields = members.iter().enumerate().map(|(index, member)| {
    let ident = &member.ident;
    let var = format_ident!("__field_{}", index);
    quote!(#ident: #var)
  });

  let body = quote! {
    let dictionary_context = || {
      ::std::borrow::Cow::Owned(format!("{} (dictionary)", context()))
    };
    let __object = ::deno_core::webidl::dictionary_object(
      value,
      &prefix,
      &dictionary_context,
    )?;
    #(#steps)*
    Ok(Self { #(#fields),* })
  };
  Ok(impl_converter(&input.ident, body))
}

fn dictionary_member(field: &syn::Field) -> syn::Result<Member> {
  let Some(ident) = field.ident.clone() else {
    return Err(syn::Error::new_spanned(
      field,
      "#[webidl(dictionary)] requires named fields",
    ));
  };
  let mut rename = None;
  let mut default = None;
  let mut required = false;
  let mut options = vec![];
  for attr in &field.attrs {
    if attr.path().is_ident("webidl") {
      attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("rename") {
          rename = Some(meta.value()?.parse::<syn::LitStr>()?.value());
          Ok(())
        } else if meta.path.is_ident("default") {
          default = Some(meta.value()?.parse::<syn::Expr>()?);
          Ok(())
        } else if meta.path.is_ident("required") {
          required = true;
          Ok(())
        } else {
          Err(meta.error(
            "unsupported #[webidl] option, expected `rename`, `default` or `required`",
          ))
        }
      })?;
    } else if attr.path().is_ident("options") {
      attr.parse_nested_meta(|meta| {
        let name =
          meta.path.get_ident().cloned().ok_or_else(|| {
            meta.error("expected the name of an options field")
          })?;
        let value: syn::Expr = meta.value()?.parse()?;
        options.push((name, value));
        Ok(())
      })?;
    }
  }
  if required && default.is_some() {
    return Err(syn::Error::new_spanned(
      &ident,
      "a dictionary member cannot be both `required` and have a `default`",
    ));
  }
  let key = rename.unwrap_or_else(|| default_key(&ident));
  Ok(Member {
    ident,
    ty: field.ty.clone(),
    key,
    default,
    required,
    options,
  })
}

fn enumeration(input: &syn::DeriveInput) -> syn::Result<TokenStream> {
  let syn::Data::Enum(data) = &input.data else {
    return Err(syn::Error::new_spanned(
      &input.ident,
      "#[webidl(enum)] can only be used on an enum",
    ));
  };
  let mut arms = vec![];
  let mut values = std::collections::HashSet::new();
  for variant in &data.variants {
    if !matches!(variant.fields, syn::Fields::Unit) {
      return Err(syn::Error::new_spanned(
        variant,
        "#[webidl(enum)] only supports unit variants",
      ));
    }
    let mut rename = None;
    for attr in &variant.attrs {
      if attr.path().is_ident("webidl") {
        attr.parse_nested_meta(|meta| {
          if meta.path.is_ident("rename") {
            rename = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            Ok(())
          } else {
            Err(meta.error("unsupported #[webidl] option, expected `rename`"))
          }
        })?;
      }
    }
    let value = rename
      .unwrap_or_else(|| stringcase::kebab_case(&variant.ident.to_string()));
    if !values.insert(value.clone()) {
      return Err(syn::Error::new_spanned(
        &variant.ident,
        format!(
          "duplicate enum value '{value}', check the variant names and #[webidl(rename)]"
        ),
      ));
    }
    let ident = &variant.ident;
    arms.push(quote!(#value => ::std::result::Result::Ok(Self::#ident),));
  }

  let name = input.ident.to_string();
  let body = quote! {
    let __string = <::std::string::String as ::deno_core::webidl::WebIdlConverter<'a>>::convert(
      scope,
      value,
      prefix.clone(),
      &context,
      &::std::default::Default::default(),
    )?;
    match __string.as_str() {
      #(#arms)*
      _ => Err(::deno_core::webidl::invalid_enum_value(
        &prefix,
        &context(),
        &__string,
        #name,
      )),
    }
  };
  Ok(impl_converter(&input.ident, body))
}

#[cfg(test)]
mod tests {
  use std::path::PathBuf;

  use quote::ToTokens;
  use syn::File;
  use syn::Item;

  #[testing_macros::fixture("webidl/test_cases/*.rs")]
  fn test_webidl_expansion(input: PathBuf) {
    let update_expected = std::env::var("UPDATE_EXPECTED").is_ok();

    let source =
      std::fs::read_to_string(&input).expect("Failed to read test file");

    const PRELUDE: &str = r"// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![allow(dead_code)]
use deno_core::WebIDL;";

    if !source.starts_with(PRELUDE) {
      panic!("Source does not start with expected prelude:\n{PRELUDE}");
    }

    let file =
      syn::parse_str::<File>(&source).expect("Failed to parse Rust file");
    let mut expected_out = vec![];
    for item in file.items {
      if let Item::Struct(_) | Item::Enum(_) = item {
        let input = syn::parse2::<syn::DeriveInput>(item.to_token_stream())
          .expect("Failed to parse derive input");
        let tokens = super::webidl(input.to_token_stream())
          .expect("Failed to generate WebIDL impl");
        let tree: File =
          syn::parse2(tokens).expect("Failed to parse generated code");
        expected_out.push(prettyplease::unparse(&tree));
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

  fn error_of(input: proc_macro2::TokenStream) -> String {
    super::webidl(input)
      .expect_err("expected the derive to fail")
      .to_string()
  }

  #[test]
  fn requires_exactly_one_kind() {
    let message =
      "#[derive(WebIDL)] takes exactly one of `dictionary` or `enum`";
    assert_eq!(
      error_of(quote::quote! {
        #[webidl(dictionary)]
        #[webidl(enum)]
        struct Foo {}
      }),
      message
    );
    assert_eq!(
      error_of(quote::quote! {
        #[webidl(enum)]
        #[webidl(enum)]
        enum Foo {}
      }),
      message
    );
    assert_eq!(
      error_of(quote::quote! {
        #[webidl(dictionary, enum)]
        struct Foo {}
      }),
      message
    );
  }

  #[test]
  fn rejects_duplicate_enum_values() {
    let error = error_of(quote::quote! {
      #[webidl(enum)]
      enum Foo {
        FooBar,
        #[webidl(rename = "foo-bar")]
        Other,
      }
    });
    assert_eq!(
      error,
      "duplicate enum value 'foo-bar', check the variant names and #[webidl(rename)]"
    );
  }

  #[test]
  fn rejects_duplicate_dictionary_keys() {
    let error = error_of(quote::quote! {
      #[webidl(dictionary)]
      struct Foo {
        foo_bar: u8,
        #[webidl(rename = "fooBar")]
        other: u8,
      }
    });
    assert_eq!(error, "duplicate dictionary key 'fooBar'");
  }
}
