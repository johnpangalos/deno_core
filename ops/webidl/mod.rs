// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! `#[derive(WebIDL)]`: implements `deno_core::webidl::WebIdlConverter` for
//! dictionaries (structs with named fields) and enumerations (enums whose
//! variants are all unit variants). Misuse is reported as a compile error.

use proc_macro2::Span;
use proc_macro2::TokenStream;
use quote::format_ident;
use quote::quote;
use syn::ext::IdentExt;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::Token;

/// Expands `#[derive(WebIDL)]` for the item in `input`.
pub fn derive(input: TokenStream) -> Result<TokenStream, syn::Error> {
  let input: syn::DeriveInput = syn::parse2(input)?;
  if parse_container(&input)? {
    derive_dictionary(&input)
  } else {
    derive_enum(&input)
  }
}

/// Reads the `#[webidl(dictionary)]` or `#[webidl(enum)]` attribute on the
/// type and returns true for a dictionary. Exactly one of them must be
/// present.
fn parse_container(input: &syn::DeriveInput) -> Result<bool, syn::Error> {
  let mut found: Vec<(bool, Span)> = vec![];
  for attr in input.attrs.iter().filter(|a| a.path().is_ident("webidl")) {
    attr.parse_nested_meta(|meta| {
      if meta.path.is_ident("dictionary") {
        found.push((true, meta.path.span()));
        Ok(())
      } else if meta.path.is_ident("enum") {
        found.push((false, meta.path.span()));
        Ok(())
      } else {
        Err(meta.error(
          "unknown #[webidl] option on a type, expected `dictionary` or `enum`",
        ))
      }
    })?;
  }
  match found.as_slice() {
    [] => Err(syn::Error::new(
      input.ident.span(),
      "#[derive(WebIDL)] requires either #[webidl(dictionary)] or #[webidl(enum)]",
    )),
    [(_, span), (_, _), ..] => Err(syn::Error::new(
      *span,
      "#[derive(WebIDL)] accepts only one of #[webidl(dictionary)] and #[webidl(enum)]",
    )),
    [(is_dictionary, _)] => Ok(*is_dictionary),
  }
}

/// The lifetime of the `WebIdlConverter` impl. A struct with one lifetime
/// parameter uses it, so that fields such as `v8::Local<'a, v8::Value>` can
/// be filled in. Otherwise `'a` is introduced.
fn impl_lifetime(
  generics: &syn::Generics,
) -> Result<(syn::Lifetime, bool), syn::Error> {
  if let Some(param) = generics.type_params().next() {
    return Err(syn::Error::new(
      param.span(),
      "#[derive(WebIDL)] does not support type parameters",
    ));
  }
  if let Some(param) = generics.const_params().next() {
    return Err(syn::Error::new(
      param.span(),
      "#[derive(WebIDL)] does not support const parameters",
    ));
  }
  let mut lifetimes = generics.lifetimes();
  match (lifetimes.next(), lifetimes.next()) {
    (None, _) => Ok((syn::Lifetime::new("'a", Span::call_site()), false)),
    (Some(param), None) => Ok((param.lifetime.clone(), true)),
    (Some(_), Some(second)) => Err(syn::Error::new(
      second.span(),
      "#[derive(WebIDL)] supports at most one lifetime parameter",
    )),
  }
}

/// Returns true if `ty` is written as `Option<...>`.
fn is_option(ty: &syn::Type) -> bool {
  match ty {
    syn::Type::Path(path) => path
      .path
      .segments
      .last()
      .is_some_and(|segment| segment.ident == "Option"),
    _ => false,
  }
}

/// Orders dictionary keys the way the spec does: by UTF-16 code units, which
/// differs from UTF-8 byte order for characters outside the BMP.
fn compare_keys(a: &str, b: &str) -> std::cmp::Ordering {
  a.encode_utf16().cmp(b.encode_utf16())
}

/// A member of a dictionary, read from a `#[webidl(...)]` and
/// `#[options(...)]` field attributes.
struct Member {
  index: usize,
  field: syn::Ident,
  key: String,
  ty: syn::Type,
  default: Option<syn::Expr>,
  required: bool,
  options: Vec<(syn::Ident, syn::Expr)>,
}

fn parse_member(
  index: usize,
  field: &syn::Field,
) -> Result<Member, syn::Error> {
  let field_ident = field.ident.clone().ok_or_else(|| {
    syn::Error::new(field.span(), "#[webidl(dictionary)] requires named fields")
  })?;
  let mut rename = None;
  let mut default = None;
  let mut required = false;
  let mut options = vec![];
  for attr in &field.attrs {
    if attr.path().is_ident("webidl") {
      attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("rename") {
          let value: syn::LitStr = meta.value()?.parse()?;
          rename = Some(value.value());
          Ok(())
        } else if meta.path.is_ident("default") {
          let value: syn::Expr = meta.value()?.parse()?;
          default = Some(value);
          Ok(())
        } else if meta.path.is_ident("required") {
          required = true;
          Ok(())
        } else {
          Err(meta.error(
            "unknown #[webidl] option on a dictionary member, expected `rename`, `default` or `required`",
          ))
        }
      })?;
    } else if attr.path().is_ident("options") {
      let pairs = attr.parse_args_with(
        Punctuated::<syn::MetaNameValue, Token![,]>::parse_terminated,
      )?;
      for pair in pairs {
        let Some(name) = pair.path.get_ident().cloned() else {
          return Err(syn::Error::new(
            pair.path.span(),
            "#[options] takes `name = value` pairs",
          ));
        };
        options.push((name, pair.value));
      }
    }
  }
  if required && default.is_some() {
    return Err(syn::Error::new(
      field.span(),
      "a dictionary member can not be both `required` and have a `default`",
    ));
  }
  Ok(Member {
    index,
    key: rename.unwrap_or_else(|| {
      stringcase::camel_case(&field_ident.unraw().to_string())
    }),
    field: field_ident,
    ty: field.ty.clone(),
    default,
    required,
    options,
  })
}

fn derive_dictionary(
  input: &syn::DeriveInput,
) -> Result<TokenStream, syn::Error> {
  let syn::Data::Struct(data) = &input.data else {
    return Err(syn::Error::new(
      input.ident.span(),
      "#[webidl(dictionary)] can only be used on a struct",
    ));
  };
  let syn::Fields::Named(fields) = &data.fields else {
    return Err(syn::Error::new(
      input.ident.span(),
      "#[webidl(dictionary)] requires a struct with named fields",
    ));
  };
  let (lifetime, has_lifetime) = impl_lifetime(&input.generics)?;
  let ty_generics = if has_lifetime {
    quote!(<#lifetime>)
  } else {
    quote!()
  };
  let name = &input.ident;
  let name_str = name.to_string();

  let members = fields
    .named
    .iter()
    .enumerate()
    .map(|(index, field)| parse_member(index, field))
    .collect::<Result<Vec<_>, _>>()?;

  for (i, member) in members.iter().enumerate() {
    if members[..i].iter().any(|other| other.key == member.key) {
      return Err(syn::Error::new(
        member.field.span(),
        format!("duplicate dictionary member key '{}'", member.key),
      ));
    }
  }

  // Members are read in lexicographic order of their keys, as the spec
  // requires, regardless of the order of the fields.
  let mut sorted: Vec<&Member> = members.iter().collect();
  sorted.sort_by(|a, b| compare_keys(&a.key, &b.key));

  let mut reads = TokenStream::new();
  for member in sorted {
    let key = &member.key;
    let ty = &member.ty;
    let value_var = format_ident!("__webidl_value_{}", member.index);
    let member_var = format_ident!("__webidl_member_{}", member.index);
    let context_var = format_ident!("__webidl_context_{}", member.index);
    let member_error = format!("can not be converted to '{name_str}' because '{key}' is required in '{name_str}'");
    let missing = match (&member.default, is_option(ty) && !member.required) {
      (Some(default), _) => quote!(#default),
      (None, true) => quote!(::std::option::Option::None),
      (None, false) => quote! {
        return ::std::result::Result::Err(
          deno_core::webidl::WebIdlError::new(
            prefix.clone(),
            || context(),
            #member_error,
          ),
        )
      },
    };
    let options = if member.options.is_empty() {
      quote!(::std::default::Default::default())
    } else {
      let names = member.options.iter().map(|(name, _)| name);
      let values = member.options.iter().map(|(_, value)| value);
      quote! {{
        let mut __options: <#ty as deno_core::webidl::WebIdlConverter<#lifetime>>::Options =
          ::std::default::Default::default();
        #(__options.#names = #values;)*
        __options
      }}
    };
    reads.extend(quote! {
      let #context_var = || {
        ::std::borrow::Cow::Owned(::std::format!(
          "{} for member '{}' of '{}'",
          context(),
          #key,
          #name_str,
        ))
      };
      let #value_var: deno_core::v8::Local<#lifetime, deno_core::v8::Value> =
        match object {
          ::std::option::Option::Some(object) => {
            let key = deno_core::v8::String::new(scope, #key).ok_or_else(|| {
              deno_core::webidl::WebIdlError::new(
                prefix.clone(),
                &#context_var,
                "threw an exception during conversion",
              )
            })?;
            object.get(scope, key.into()).ok_or_else(|| {
              deno_core::webidl::WebIdlError::new(
                prefix.clone(),
                &#context_var,
                "threw an exception during conversion",
              )
            })?
          }
          ::std::option::Option::None => deno_core::v8::undefined(scope).into(),
        };
      let #member_var: #ty = if #value_var.is_undefined() {
        #missing
      } else {
        <#ty as deno_core::webidl::WebIdlConverter<#lifetime>>::convert(
          scope,
          #value_var,
          prefix.clone(),
          &#context_var,
          &#options,
        )?
      };
    });
  }

  let field_names = members.iter().map(|m| &m.field);
  let member_vars = members
    .iter()
    .map(|m| format_ident!("__webidl_member_{}", m.index));

  Ok(quote! {
    impl<#lifetime> deno_core::webidl::WebIdlConverter<#lifetime> for #name #ty_generics {
      type Options = ();

      fn convert<C>(
        scope: &mut deno_core::v8::HandleScope<#lifetime>,
        value: deno_core::v8::Local<#lifetime, deno_core::v8::Value>,
        prefix: ::std::borrow::Cow<'static, str>,
        context: C,
        options: &Self::Options,
      ) -> ::std::result::Result<Self, deno_core::webidl::WebIdlError>
      where
        C: Fn() -> ::std::borrow::Cow<'static, str>,
      {
        let _ = options;
        // undefined and null are treated as an empty dictionary, so every
        // member is missing.
        let object = if value.is_null_or_undefined() {
          ::std::option::Option::None
        } else {
          match deno_core::v8::Local::<deno_core::v8::Object>::try_from(value) {
            ::std::result::Result::Ok(object) => ::std::option::Option::Some(object),
            ::std::result::Result::Err(_) => {
              return ::std::result::Result::Err(
                deno_core::webidl::WebIdlError::new(
                  prefix.clone(),
                  || context(),
                  ::std::format!("can not be converted to '{}' because it is not an object", #name_str),
                ),
              )
            }
          }
        };
        let _ = &object;
        #reads
        ::std::result::Result::Ok(Self {
          #(#field_names: #member_vars,)*
        })
      }
    }
  })
}

fn derive_enum(input: &syn::DeriveInput) -> Result<TokenStream, syn::Error> {
  let syn::Data::Enum(data) = &input.data else {
    return Err(syn::Error::new(
      input.ident.span(),
      "#[webidl(enum)] can only be used on an enum",
    ));
  };
  if !input.generics.params.is_empty() {
    return Err(syn::Error::new(
      input.generics.span(),
      "#[webidl(enum)] does not support generic parameters",
    ));
  }
  if data.variants.is_empty() {
    return Err(syn::Error::new(
      input.ident.span(),
      "#[webidl(enum)] requires at least one variant",
    ));
  }
  let name = &input.ident;
  let name_str = name.to_string();
  let mut arms = TokenStream::new();
  let mut values: Vec<String> = vec![];
  for variant in &data.variants {
    if !matches!(variant.fields, syn::Fields::Unit) {
      return Err(syn::Error::new(
        variant.span(),
        "#[webidl(enum)] requires every variant to be a unit variant",
      ));
    }
    let mut rename = None;
    for attr in variant.attrs.iter().filter(|a| a.path().is_ident("webidl")) {
      attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("rename") {
          let value: syn::LitStr = meta.value()?.parse()?;
          rename = Some(value.value());
          Ok(())
        } else {
          Err(meta.error(
            "unknown #[webidl] option on an enum variant, expected `rename`",
          ))
        }
      })?;
    }
    let value = rename.unwrap_or_else(|| {
      stringcase::kebab_case(&variant.ident.unraw().to_string())
    });
    if values.contains(&value) {
      return Err(syn::Error::new(
        variant.span(),
        format!("duplicate enumeration value '{value}'"),
      ));
    }
    values.push(value.clone());
    let ident = &variant.ident;
    arms.extend(quote!(#value => ::std::result::Result::Ok(Self::#ident),));
  }

  Ok(quote! {
    impl<'a> deno_core::webidl::WebIdlConverter<'a> for #name {
      type Options = ();

      fn convert<C>(
        scope: &mut deno_core::v8::HandleScope<'a>,
        value: deno_core::v8::Local<'a, deno_core::v8::Value>,
        prefix: ::std::borrow::Cow<'static, str>,
        context: C,
        options: &Self::Options,
      ) -> ::std::result::Result<Self, deno_core::webidl::WebIdlError>
      where
        C: Fn() -> ::std::borrow::Cow<'static, str>,
      {
        let _ = options;
        let string = <::std::string::String as deno_core::webidl::WebIdlConverter<'a>>::convert(
          scope,
          value,
          prefix.clone(),
          &context,
          &::std::default::Default::default(),
        )?;
        match string.as_str() {
          #arms
          _ => ::std::result::Result::Err(deno_core::webidl::WebIdlError::new(
            prefix,
            || context(),
            ::std::format!(
              "is not a valid value for enumeration '{}': '{}'",
              #name_str,
              string,
            ),
          )),
        }
      }
    }
  })
}

#[cfg(test)]
mod tests {
  use super::derive;
  use pretty_assertions::assert_eq;
  use quote::quote;
  use quote::ToTokens;
  use std::path::PathBuf;
  use syn::Item;

  #[testing_macros::fixture("webidl/test_cases/*.rs")]
  fn test_derive_expansion(input: PathBuf) {
    let update_expected = std::env::var("UPDATE_EXPECTED").is_ok();
    let source =
      std::fs::read_to_string(&input).expect("Failed to read test file");
    let file =
      syn::parse_str::<syn::File>(&source).expect("Failed to parse Rust file");

    let mut expected_out = vec![];
    for mut item in file.items {
      let attrs = match &mut item {
        Item::Struct(item) => &mut item.attrs,
        Item::Enum(item) => &mut item.attrs,
        _ => continue,
      };
      let Some(index) = attrs.iter().position(|attr| {
        attr.path().is_ident("derive")
          && attr.to_token_stream().to_string().contains("WebIDL")
      }) else {
        continue;
      };
      attrs.remove(index);
      let tokens = derive(item.to_token_stream()).expect("Failed to expand");
      let tree = syn::parse2(tokens).unwrap();
      expected_out.push(prettyplease::unparse(&tree));
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

  fn error_for(input: proc_macro2::TokenStream) -> String {
    match derive(input) {
      Ok(output) => panic!("expected an error, got: {output}"),
      Err(error) => error.to_string(),
    }
  }

  #[test]
  fn test_requires_kind() {
    assert_eq!(
      error_for(quote! { struct Foo { a: u8 } }),
      "#[derive(WebIDL)] requires either #[webidl(dictionary)] or #[webidl(enum)]"
    );
  }

  #[test]
  fn test_rejects_both_kinds() {
    assert_eq!(
      error_for(quote! {
        #[webidl(dictionary)]
        #[webidl(enum)]
        struct Foo { a: u8 }
      }),
      "#[derive(WebIDL)] accepts only one of #[webidl(dictionary)] and #[webidl(enum)]"
    );
  }

  #[test]
  fn test_dictionary_misuse() {
    assert_eq!(
      error_for(quote! { #[webidl(dictionary)] struct Foo(u8); }),
      "#[webidl(dictionary)] requires a struct with named fields"
    );
    assert_eq!(
      error_for(quote! { #[webidl(dictionary)] struct Foo; }),
      "#[webidl(dictionary)] requires a struct with named fields"
    );
    assert_eq!(
      error_for(quote! { #[webidl(dictionary)] enum Foo { A } }),
      "#[webidl(dictionary)] can only be used on a struct"
    );
    assert_eq!(
      error_for(quote! { #[webidl(dictionary)] struct Foo<T> { a: T } }),
      "#[derive(WebIDL)] does not support type parameters"
    );
    assert_eq!(
      error_for(quote! {
        #[webidl(dictionary)]
        struct Foo { #[webidl(required, default = 1)] a: u8 }
      }),
      "a dictionary member can not be both `required` and have a `default`"
    );
    assert_eq!(
      error_for(quote! {
        #[webidl(dictionary)]
        struct Foo { #[webidl(renamed = "x")] a: u8 }
      }),
      "unknown #[webidl] option on a dictionary member, expected `rename`, `default` or `required`"
    );
    assert_eq!(
      error_for(quote! {
        #[webidl(dictionary)]
        struct Foo { #[options(1)] a: u8 }
      }),
      "expected identifier"
    );
    assert_eq!(
      error_for(quote! { #[webidl(dictionary, bogus)] struct Foo { a: u8 } }),
      "unknown #[webidl] option on a type, expected `dictionary` or `enum`"
    );
  }

  #[test]
  fn test_enum_misuse() {
    assert_eq!(
      error_for(quote! { #[webidl(enum)] struct Foo { a: u8 } }),
      "#[webidl(enum)] can only be used on an enum"
    );
    assert_eq!(
      error_for(quote! { #[webidl(enum)] enum Foo { A(u8) } }),
      "#[webidl(enum)] requires every variant to be a unit variant"
    );
    assert_eq!(
      error_for(quote! { #[webidl(enum)] enum Foo { A { x: u8 } } }),
      "#[webidl(enum)] requires every variant to be a unit variant"
    );
    assert_eq!(
      error_for(quote! { #[webidl(enum)] enum Foo<'a> { A } }),
      "#[webidl(enum)] does not support generic parameters"
    );
    assert_eq!(
      error_for(quote! { #[webidl(enum)] enum Foo {} }),
      "#[webidl(enum)] requires at least one variant"
    );
    assert_eq!(
      error_for(quote! { #[webidl(enum)] enum Foo { #[webidl(bogus)] A } }),
      "unknown #[webidl] option on an enum variant, expected `rename`"
    );
  }

  #[test]
  fn test_rejects_duplicates() {
    assert_eq!(
      error_for(quote! {
        #[webidl(dictionary)]
        struct Foo { foo_bar: u8, #[webidl(rename = "fooBar")] other: u8 }
      }),
      "duplicate dictionary member key 'fooBar'"
    );
    assert_eq!(
      error_for(quote! {
        #[webidl(enum)]
        enum Foo { Foo, #[webidl(rename = "foo")] Bar }
      }),
      "duplicate enumeration value 'foo'"
    );
  }

  #[test]
  fn test_key_order_is_utf16() {
    use super::compare_keys;
    use std::cmp::Ordering;
    assert_eq!(compare_keys("\u{10000}", "\u{FFFD}"), Ordering::Less);
    assert_eq!(compare_keys("a", "b"), Ordering::Less);
    assert_eq!(compare_keys("B", "a"), Ordering::Less);
  }

  #[test]
  fn test_accepts_valid_input() {
    assert!(derive(quote! {
      #[webidl(dictionary)]
      struct Foo<'a> {
        foo_bar: u8,
        #[webidl(rename = "x", default = 1)]
        other: u8,
        #[options(clamp = true)]
        clamped: Option<u8>,
        value: deno_core::v8::Local<'a, deno_core::v8::Value>,
      }
    })
    .is_ok());
    assert!(derive(quote! {
      #[webidl(enum)]
      enum Foo { FooBar, #[webidl(rename = "value")] Other }
    })
    .is_ok());
  }
}
