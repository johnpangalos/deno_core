// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! Implementation of `#[derive(WebIDL)]`.

use proc_macro2::Ident;
use proc_macro2::Span;
use proc_macro2::TokenStream;
use quote::quote;
use syn::spanned::Spanned;
use syn::Data;
use syn::DataEnum;
use syn::DataStruct;
use syn::DeriveInput;
use syn::Error;
use syn::Expr;
use syn::Fields;
use syn::GenericParam;
use syn::Lifetime;
use syn::LitStr;
use syn::Type;

enum Kind {
  Dictionary,
  Enum,
}

pub fn webidl(item: TokenStream) -> Result<TokenStream, Error> {
  let input = syn::parse2::<DeriveInput>(item)?;

  let mut kind = None;
  for attr in input.attrs.iter().filter(|a| a.path().is_ident("webidl")) {
    attr.parse_nested_meta(|meta| {
      if meta.path.is_ident("dictionary") {
        kind = Some(Kind::Dictionary);
      } else if meta.path.is_ident("enum") {
        kind = Some(Kind::Enum);
      } else {
        return Err(
          meta
            .error("unknown webidl attribute, expected `dictionary` or `enum`"),
        );
      }
      Ok(())
    })?;
  }

  let lifetime = converter_lifetime(&input)?;

  match (kind, &input.data) {
    (Some(Kind::Dictionary), Data::Struct(data)) => {
      dictionary(&input, data, lifetime)
    }
    (Some(Kind::Enum), Data::Enum(data)) => r#enum(&input, data, lifetime),
    (Some(Kind::Dictionary), _) => Err(Error::new(
      input.ident.span(),
      "#[webidl(dictionary)] can only be used on structs",
    )),
    (Some(Kind::Enum), _) => Err(Error::new(
      input.ident.span(),
      "#[webidl(enum)] can only be used on enums",
    )),
    (None, _) => Err(Error::new(
      input.ident.span(),
      "expected #[webidl(dictionary)] or #[webidl(enum)]",
    )),
  }
}

/// The lifetime of the `WebIdlConverter` implementation, and whether it
/// is a lifetime of the type itself.
fn converter_lifetime(input: &DeriveInput) -> Result<(Lifetime, bool), Error> {
  let mut lifetime = None;
  for param in &input.generics.params {
    match param {
      GenericParam::Lifetime(param) if lifetime.is_none() => {
        lifetime = Some(param.lifetime.clone());
      }
      _ => {
        return Err(Error::new(
          param.span(),
          "#[derive(WebIDL)] only supports types with at most one lifetime parameter",
        ))
      }
    }
  }
  Ok(match lifetime {
    Some(lifetime) => (lifetime, true),
    None => (Lifetime::new("'a", Span::call_site()), false),
  })
}

fn impl_converter(
  input: &DeriveInput,
  (lifetime, own_lifetime): (Lifetime, bool),
  body: TokenStream,
) -> TokenStream {
  let ident = &input.ident;
  let self_ty = if own_lifetime {
    quote!(#ident<#lifetime>)
  } else {
    quote!(#ident)
  };
  quote! {
    impl<#lifetime> deno_core::webidl::WebIdlConverter<#lifetime> for #self_ty {
      type Options = ();

      fn convert<C>(
        __scope: &mut deno_core::v8::HandleScope<#lifetime>,
        __value: deno_core::v8::Local<#lifetime, deno_core::v8::Value>,
        __prefix: std::borrow::Cow<'static, str>,
        __context: C,
        _: &Self::Options,
      ) -> Result<Self, deno_core::webidl::WebIdlError>
      where
        C: Fn() -> std::borrow::Cow<'static, str>,
      {
        #body
      }
    }
  }
}

fn parse_rename(meta: &syn::meta::ParseNestedMeta) -> Result<String, Error> {
  Ok(meta.value()?.parse::<LitStr>()?.value())
}

struct DictionaryMember {
  ident: Ident,
  key: String,
  ty: Type,
  default_value: Option<Expr>,
  required: bool,
  options: Vec<(Ident, Expr)>,
}

impl DictionaryMember {
  fn parse(field: &syn::Field) -> Result<Self, Error> {
    let ident = field.ident.clone().unwrap();
    let mut key = None;
    let mut default_value = None;
    let mut required = false;
    let mut options = vec![];

    for attr in &field.attrs {
      if attr.path().is_ident("webidl") {
        attr.parse_nested_meta(|meta| {
          if meta.path.is_ident("rename") {
            key = Some(parse_rename(&meta)?);
          } else if meta.path.is_ident("default") {
            default_value = Some(meta.value()?.parse::<Expr>()?);
          } else if meta.path.is_ident("required") {
            required = true;
          } else {
            return Err(meta.error(
              "unknown webidl member attribute, expected `rename`, `default` or `required`",
            ));
          }
          Ok(())
        })?;
      } else if attr.path().is_ident("options") {
        attr.parse_nested_meta(|meta| {
          let option = meta.path.require_ident()?.clone();
          let value = meta.value()?.parse::<Expr>()?;
          options.push((option, value));
          Ok(())
        })?;
      }
    }

    if required && default_value.is_some() {
      return Err(Error::new(
        ident.span(),
        "a required dictionary member can not have a default value",
      ));
    }

    let key = key.unwrap_or_else(|| stringcase::camel_case(&ident.to_string()));

    Ok(Self {
      ident,
      key,
      ty: field.ty.clone(),
      default_value,
      required,
      options,
    })
  }

  fn is_option(&self) -> bool {
    match &self.ty {
      Type::Path(path) => path
        .path
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "Option"),
      _ => false,
    }
  }
}

fn dictionary(
  input: &DeriveInput,
  data: &DataStruct,
  lifetime: (Lifetime, bool),
) -> Result<TokenStream, Error> {
  let Fields::Named(fields) = &data.fields else {
    return Err(Error::new(
      input.ident.span(),
      "#[webidl(dictionary)] can only be used on structs with named fields",
    ));
  };

  let mut members = fields
    .named
    .iter()
    .map(DictionaryMember::parse)
    .collect::<Result<Vec<_>, _>>()?;

  // Members are converted in lexicographical order of their keys.
  members.sort_by(|a, b| a.key.cmp(&b.key));
  if let Some(pair) = members.windows(2).find(|pair| pair[0].key == pair[1].key)
  {
    return Err(Error::new(
      pair[1].ident.span(),
      format!("duplicate dictionary member '{}'", pair[1].key),
    ));
  }

  let name = input.ident.to_string();
  let lt = &lifetime.0;

  let conversions = members.iter().map(|member| {
    let DictionaryMember { ident, key, ty, .. } = member;
    let var = quote::format_ident!("__member_{}", ident);

    let options = if member.options.is_empty() {
      quote!(&Default::default())
    } else {
      let assignments = member
        .options
        .iter()
        .map(|(option, value)| quote!(__options.#option = #value;));
      quote! {
        &{
          #[allow(clippy::field_reassign_with_default)]
          let mut __options: <#ty as deno_core::webidl::WebIdlConverter<#lt>>::Options = Default::default();
          #(#assignments)*
          __options
        }
      }
    };

    let missing = if let Some(default_value) = &member.default_value {
      quote!(#default_value)
    } else if member.is_option() && !member.required {
      quote!(None)
    } else {
      quote! {
        return Err(deno_core::webidl::WebIdlError::new(
          __prefix,
          &__context,
          deno_core::webidl::WebIdlErrorKind::DictionaryCannotConvertKey {
            converter: #name,
            key: #key,
          },
        ))
      }
    };

    quote! {
      let #var = match deno_core::webidl::__private::dictionary_member(
        __scope,
        __object,
        #key,
        &__prefix,
        &__context,
      )? {
        Some(__member) => <#ty as deno_core::webidl::WebIdlConverter<#lt>>::convert(
          __scope,
          __member,
          __prefix.clone(),
          || deno_core::webidl::__private::dictionary_member_context(#name, #key, &__context),
          #options,
        )?,
        None => #missing,
      };
    }
  });

  let fields = members.iter().map(|member| {
    let ident = &member.ident;
    let var = quote::format_ident!("__member_{}", ident);
    quote!(#ident: #var)
  });

  let body = quote! {
    let __object = deno_core::webidl::__private::dictionary_object(
      __value,
      &__prefix,
      &__context,
    )?;
    #(#conversions)*
    Ok(Self { #(#fields),* })
  };

  Ok(impl_converter(input, lifetime, body))
}

fn r#enum(
  input: &DeriveInput,
  data: &DataEnum,
  lifetime: (Lifetime, bool),
) -> Result<TokenStream, Error> {
  let mut variants = vec![];
  for variant in &data.variants {
    if !matches!(variant.fields, Fields::Unit) {
      return Err(Error::new(
        variant.ident.span(),
        "#[webidl(enum)] variants can not have fields",
      ));
    }

    let mut value = None;
    for attr in variant.attrs.iter().filter(|a| a.path().is_ident("webidl")) {
      attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("rename") {
          value = Some(parse_rename(&meta)?);
        } else {
          return Err(
            meta.error("unknown webidl variant attribute, expected `rename`"),
          );
        }
        Ok(())
      })?;
    }
    let value = value
      .unwrap_or_else(|| stringcase::kebab_case(&variant.ident.to_string()));

    if variants.iter().any(|(_, v)| v == &value) {
      return Err(Error::new(
        variant.ident.span(),
        format!("duplicate enum value '{value}'"),
      ));
    }
    variants.push((&variant.ident, value));
  }

  if variants.is_empty() {
    return Err(Error::new(
      input.ident.span(),
      "#[webidl(enum)] must have at least one variant",
    ));
  }

  let name = input.ident.to_string();
  let lt = &lifetime.0;
  let arms = variants
    .iter()
    .map(|(ident, value)| quote!(#value => Self::#ident,));

  let body = quote! {
    let __value = <String as deno_core::webidl::WebIdlConverter<#lt>>::convert(
      __scope,
      __value,
      __prefix.clone(),
      &__context,
      &Default::default(),
    )?;
    Ok(match __value.as_str() {
      #(#arms)*
      _ => {
        return Err(deno_core::webidl::WebIdlError::new(
          __prefix,
          &__context,
          deno_core::webidl::WebIdlErrorKind::InvalidEnumVariant {
            converter: #name,
            variant: __value.clone(),
          },
        ))
      }
    })
  };

  Ok(impl_converter(input, lifetime, body))
}

#[cfg(test)]
mod tests {
  use super::*;
  use quote::ToTokens;
  use std::path::PathBuf;
  use syn::Attribute;
  use syn::File;
  use syn::Item;

  fn is_derive_webidl(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
      attr.path().is_ident("derive")
        && attr.meta.to_token_stream().to_string().contains("WebIDL")
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
        Item::Struct(item) if is_derive_webidl(&item.attrs) => {
          expected_out.push(expand_webidl(item));
        }
        Item::Enum(item) if is_derive_webidl(&item.attrs) => {
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

  fn expand_err(source: &str) -> String {
    webidl(syn::parse_str(source).unwrap())
      .unwrap_err()
      .to_string()
  }

  #[test]
  fn test_errors() {
    assert_eq!(
      expand_err("struct Foo { a: u8 }"),
      "expected #[webidl(dictionary)] or #[webidl(enum)]"
    );
    assert_eq!(
      expand_err("#[webidl(dictionary)] enum Foo { A }"),
      "#[webidl(dictionary)] can only be used on structs"
    );
    assert_eq!(
      expand_err("#[webidl(enum)] struct Foo { a: u8 }"),
      "#[webidl(enum)] can only be used on enums"
    );
    assert_eq!(
      expand_err("#[webidl(dictionary)] struct Foo(u8);"),
      "#[webidl(dictionary)] can only be used on structs with named fields"
    );
    assert_eq!(
      expand_err("#[webidl(dictionary)] struct Foo<T> { a: T }"),
      "#[derive(WebIDL)] only supports types with at most one lifetime parameter"
    );
    assert_eq!(
      expand_err(
        "#[webidl(dictionary)] struct Foo { #[webidl(required, default = 1)] a: u8 }"
      ),
      "a required dictionary member can not have a default value"
    );
    assert_eq!(
      expand_err(
        "#[webidl(dictionary)] struct Foo { foo_bar: u8, #[webidl(rename = \"fooBar\")] b: u8 }"
      ),
      "duplicate dictionary member 'fooBar'"
    );
    assert_eq!(
      expand_err("#[webidl(enum)] enum Foo { A(u8) }"),
      "#[webidl(enum)] variants can not have fields"
    );
    assert_eq!(
      expand_err("#[webidl(enum)] enum Foo { FooBar, #[webidl(rename = \"foo-bar\")] Baz }"),
      "duplicate enum value 'foo-bar'"
    );
    assert_eq!(
      expand_err("#[webidl(enum)] enum Foo {}"),
      "#[webidl(enum)] must have at least one variant"
    );
    assert!(expand_err("#[webidl(foo)] struct Foo {}")
      .starts_with("unknown webidl attribute"));
  }
}
