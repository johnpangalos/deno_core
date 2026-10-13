// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! `#[derive(WebIDL)]`, which implements `deno_core::webidl::WebIdlConverter`
//! for WebIDL dictionaries and enums.

use proc_macro2::Span;
use proc_macro2::TokenStream;
use quote::format_ident;
use quote::quote;
use syn::ext::IdentExt;
use syn::parse2;
use syn::parse_quote;
use syn::spanned::Spanned;
use syn::Attribute;
use syn::Data;
use syn::DeriveInput;
use syn::Error;
use syn::Expr;
use syn::Fields;
use syn::GenericParam;
use syn::Ident;
use syn::Lifetime;
use syn::LitStr;
use syn::Type;

enum Kind {
  Dictionary,
  Enum,
}

pub fn webidl(item: TokenStream) -> Result<TokenStream, Error> {
  let input = parse2::<DeriveInput>(item)?;
  let kind = parse_kind(&input)?;

  // The converter's lifetime is the type's own lifetime if it has one, so
  // members can hold `v8::Local`s.
  let mut impl_generics = input.generics.clone();
  let lifetime = match input.generics.lifetimes().next() {
    Some(param) => param.lifetime.clone(),
    None => {
      let lifetime = Lifetime::new("'a", Span::call_site());
      impl_generics
        .params
        .insert(0, GenericParam::Lifetime(parse_quote!(#lifetime)));
      lifetime
    }
  };
  let (impl_generics, _, _) = impl_generics.split_for_impl();
  let (_, ty_generics, where_clause) = input.generics.split_for_impl();

  let ident = &input.ident;
  let name = ident.unraw().to_string();
  let body = match kind {
    Kind::Dictionary => dictionary(&input, &name, &lifetime)?,
    Kind::Enum => enumeration(&input, &name)?,
  };

  Ok(quote! {
    impl #impl_generics ::deno_core::webidl::WebIdlConverter<#lifetime> for #ident #ty_generics #where_clause {
      type Options = ();

      #[allow(unused_variables)]
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

/// Parses `#[webidl(dictionary)]` or `#[webidl(enum)]` on the type.
fn parse_kind(input: &DeriveInput) -> Result<Kind, Error> {
  let mut kind = None;
  for attr in input.attrs.iter().filter(|a| is_webidl(a)) {
    let ident = attr.parse_args_with(Ident::parse_any)?;
    kind = Some(match ident.to_string().as_str() {
      "dictionary" => Kind::Dictionary,
      "enum" => Kind::Enum,
      _ => {
        return Err(Error::new(
          ident.span(),
          "expected #[webidl(dictionary)] or #[webidl(enum)]",
        ))
      }
    });
  }
  kind.ok_or_else(|| {
    Error::new(
      input.ident.span(),
      "#[derive(WebIDL)] requires #[webidl(dictionary)] or #[webidl(enum)]",
    )
  })
}

struct DictionaryMember {
  ident: Ident,
  ty: Type,
  key: String,
  default: Option<Expr>,
  required: bool,
  options: Vec<(Ident, Expr)>,
}

impl DictionaryMember {
  fn parse(field: &syn::Field) -> Result<Self, Error> {
    let ident = field.ident.clone().unwrap();
    let mut member = DictionaryMember {
      key: stringcase::camel_case(&ident.unraw().to_string()),
      ident,
      ty: field.ty.clone(),
      default: None,
      required: false,
      options: vec![],
    };

    for attr in &field.attrs {
      if is_webidl(attr) {
        attr.parse_nested_meta(|meta| {
          if meta.path.is_ident("rename") {
            member.key = meta.value()?.parse::<LitStr>()?.value();
          } else if meta.path.is_ident("default") {
            member.default = Some(meta.value()?.parse()?);
          } else if meta.path.is_ident("required") {
            member.required = true;
          } else {
            return Err(meta.error(
              "expected `rename = \"...\"`, `default = ...` or `required`",
            ));
          }
          Ok(())
        })?;
      } else if attr.path().is_ident("options") {
        attr.parse_nested_meta(|meta| {
          let Some(option) = meta.path.get_ident() else {
            return Err(meta.error("expected an option name"));
          };
          let option = option.clone();
          member.options.push((option, meta.value()?.parse()?));
          Ok(())
        })?;
      }
    }

    if member.required && member.default.is_some() {
      return Err(Error::new(
        member.ident.span(),
        "a required dictionary member can not have a default",
      ));
    }
    Ok(member)
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
  name: &str,
  lifetime: &Lifetime,
) -> Result<TokenStream, Error> {
  let Data::Struct(data) = &input.data else {
    return Err(Error::new(
      input.ident.span(),
      "#[webidl(dictionary)] can only be used on a struct",
    ));
  };
  let Fields::Named(fields) = &data.fields else {
    return Err(Error::new(
      data.fields.span(),
      "#[webidl(dictionary)] requires a struct with named fields",
    ));
  };

  let mut members = fields
    .named
    .iter()
    .map(DictionaryMember::parse)
    .collect::<Result<Vec<_>, _>>()?;
  // Dictionary members are converted in lexicographical order of their keys.
  members.sort_by(|a, b| a.key.cmp(&b.key));

  let conversions = members.iter().map(|member| {
    let DictionaryMember { ident, ty, key, .. } = member;
    let var = format_ident!("__member_{}", ident.unraw());
    let converter =
      quote!(<#ty as ::deno_core::webidl::WebIdlConverter<#lifetime>>);

    let options = if member.options.is_empty() {
      quote!(::std::default::Default::default())
    } else {
      let assignments = member
        .options
        .iter()
        .map(|(option, value)| quote!(__options.#option = #value;));
      quote! {{
        #[allow(clippy::field_reassign_with_default)]
        let mut __options: #converter::Options = ::std::default::Default::default();
        #(#assignments)*
        __options
      }}
    };

    let missing = if let Some(default) = &member.default {
      quote!(#default)
    } else if member.is_option() && !member.required {
      quote!(::std::option::Option::None)
    } else {
      quote! {
        return ::std::result::Result::Err(::deno_core::webidl::WebIdlError::new(
          __prefix,
          &__context,
          ::deno_core::webidl::WebIdlErrorKind::DictionaryCannotConvertKey {
            converter: #name,
            key: #key,
          },
        ))
      }
    };

    quote! {
      let #var = match ::deno_core::webidl::dictionary_member(__scope, __object, #key, &__prefix, &__context)? {
        ::std::option::Option::Some(__member) => {
          let __options = #options;
          #converter::convert(
            __scope,
            __member,
            __prefix.clone(),
            || ::std::borrow::Cow::Owned(format!("'{}' of '{}' ({})", #key, #name, __context())),
            &__options,
          )?
        }
        ::std::option::Option::None => #missing,
      };
    }
  });

  let field_inits = members.iter().map(|member| {
    let ident = &member.ident;
    let var = format_ident!("__member_{}", ident.unraw());
    quote!(#ident: #var)
  });

  Ok(quote! {
    let __object = ::deno_core::webidl::dictionary_object(__value, #name, &__prefix, &__context)?;
    #(#conversions)*
    ::std::result::Result::Ok(Self {
      #(#field_inits),*
    })
  })
}

fn enumeration(input: &DeriveInput, name: &str) -> Result<TokenStream, Error> {
  let Data::Enum(data) = &input.data else {
    return Err(Error::new(
      input.ident.span(),
      "#[webidl(enum)] can only be used on an enum",
    ));
  };

  let arms = data
    .variants
    .iter()
    .map(|variant| {
      if !matches!(variant.fields, Fields::Unit) {
        return Err(Error::new(
          variant.span(),
          "#[webidl(enum)] requires unit variants",
        ));
      }
      let mut value =
        stringcase::kebab_case(&variant.ident.unraw().to_string());
      for attr in variant.attrs.iter().filter(|a| is_webidl(a)) {
        attr.parse_nested_meta(|meta| {
          if meta.path.is_ident("rename") {
            value = meta.value()?.parse::<LitStr>()?.value();
            Ok(())
          } else {
            Err(meta.error("expected `rename = \"...\"`"))
          }
        })?;
      }
      let ident = &variant.ident;
      Ok(quote!(#value => ::std::result::Result::Ok(Self::#ident),))
    })
    .collect::<Result<Vec<_>, Error>>()?;

  Ok(quote! {
    let __string = <::std::string::String as ::deno_core::webidl::WebIdlConverter>::convert(
      __scope,
      __value,
      __prefix.clone(),
      &__context,
      &::std::default::Default::default(),
    )?;
    match __string.as_str() {
      #(#arms)*
      _ => ::std::result::Result::Err(::deno_core::webidl::WebIdlError::new(
        __prefix,
        &__context,
        ::deno_core::webidl::WebIdlErrorKind::InvalidEnumVariant {
          converter: #name,
          variant: __string,
        },
      )),
    }
  })
}

fn is_webidl(attr: &Attribute) -> bool {
  attr.path().is_ident("webidl")
}

#[cfg(test)]
mod tests {
  use super::*;
  use quote::ToTokens;
  use std::path::PathBuf;
  use syn::File;
  use syn::Item;

  fn derives_webidl(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
      attr.path().is_ident("derive")
        && attr.to_token_stream().to_string().contains("WebIDL")
    })
  }

  #[testing_macros::fixture("webidl/test_cases/*.rs")]
  fn test_proc_macro_output(input: PathBuf) {
    let update_expected = std::env::var("UPDATE_EXPECTED").is_ok();

    let source =
      std::fs::read_to_string(&input).expect("Failed to read test file");
    let file =
      syn::parse_str::<File>(&source).expect("Failed to parse Rust file");
    let mut expected_out = vec![];
    for item in file.items {
      let (attrs, tokens) = match &item {
        Item::Struct(item) => (&item.attrs, item.to_token_stream()),
        Item::Enum(item) => (&item.attrs, item.to_token_stream()),
        _ => continue,
      };
      if !derives_webidl(attrs) {
        continue;
      }
      let tokens = webidl(tokens).expect("Failed to derive WebIDL");
      println!("======== Raw tokens ========:\n{}", tokens.clone());
      let tree = syn::parse2(tokens).unwrap();
      let actual = prettyplease::unparse(&tree);
      println!("======== Generated ========:\n{}", actual);
      expected_out.push(actual);
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

  fn derive_err(item: TokenStream) -> String {
    webidl(item).unwrap_err().to_string()
  }

  #[test]
  fn test_errors() {
    assert_eq!(
      derive_err(quote!(
        struct Foo {
          a: u8,
        }
      )),
      "#[derive(WebIDL)] requires #[webidl(dictionary)] or #[webidl(enum)]"
    );
    assert_eq!(
      derive_err(quote!(
        #[webidl(union)]
        struct Foo {
          a: u8,
        }
      )),
      "expected #[webidl(dictionary)] or #[webidl(enum)]"
    );
    assert_eq!(
      derive_err(quote!(
        #[webidl(dictionary)]
        struct Foo(u8);
      )),
      "#[webidl(dictionary)] requires a struct with named fields"
    );
    assert_eq!(
      derive_err(quote!(
        #[webidl(dictionary)]
        enum Foo {
          A,
        }
      )),
      "#[webidl(dictionary)] can only be used on a struct"
    );
    assert_eq!(
      derive_err(quote!(
        #[webidl(enum)]
        enum Foo {
          A(u8),
        }
      )),
      "#[webidl(enum)] requires unit variants"
    );
    assert_eq!(
      derive_err(quote!(
        #[webidl(dictionary)]
        struct Foo {
          #[webidl(required, default = 1)]
          a: u8,
        }
      )),
      "a required dictionary member can not have a default"
    );
    assert_eq!(
      derive_err(quote!(
        #[webidl(dictionary)]
        struct Foo {
          #[webidl(bogus)]
          a: u8,
        }
      )),
      "expected `rename = \"...\"`, `default = ...` or `required`"
    );
  }

  #[test]
  fn test_case_conversion() {
    assert_eq!(stringcase::camel_case("foo_bar_baz"), "fooBarBaz");
    assert_eq!(stringcase::kebab_case("FooBar"), "foo-bar");
  }
}
