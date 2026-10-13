// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! `#[derive(WebIDL)]`. Generates `deno_core::webidl::WebIdlConverter`
//! implementations for dictionaries (structs with named fields) and enums of
//! unit variants. See the `deno_core::webidl` module docs for the rules.

use std::collections::HashSet;

use proc_macro2::Span;
use proc_macro2::TokenStream;
use quote::format_ident;
use quote::quote;
use syn::ext::IdentExt;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::Data;
use syn::DeriveInput;
use syn::Fields;
use syn::Token;

pub fn webidl(input: TokenStream) -> Result<TokenStream, syn::Error> {
  let input = syn::parse2::<DeriveInput>(input)?;
  let kind = container_kind(&input)?;
  if !input.generics.params.is_empty() {
    return Err(syn::Error::new_spanned(
      &input.generics,
      "#[derive(WebIDL)] does not support generic types",
    ));
  }
  match (kind, &input.data) {
    (ContainerKind::Dictionary, Data::Struct(data)) => dictionary(&input, data),
    (ContainerKind::Enum, Data::Enum(data)) => enumeration(&input, data),
    (ContainerKind::Dictionary, _) => Err(syn::Error::new(
      input.ident.span(),
      "#[webidl(dictionary)] can only be used on a struct",
    )),
    (ContainerKind::Enum, _) => Err(syn::Error::new(
      input.ident.span(),
      "#[webidl(enum)] can only be used on an enum",
    )),
  }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ContainerKind {
  Dictionary,
  Enum,
}

fn container_kind(input: &DeriveInput) -> Result<ContainerKind, syn::Error> {
  let mut kind = None;
  for attr in input
    .attrs
    .iter()
    .filter(|attr| attr.path().is_ident("webidl"))
  {
    attr.parse_nested_meta(|meta| {
      let parsed = if meta.path.is_ident("dictionary") {
        ContainerKind::Dictionary
      } else if meta.path.is_ident("enum") {
        ContainerKind::Enum
      } else {
        return Err(meta.error(
          "unsupported #[webidl] key on a type, expected `dictionary` or `enum`",
        ));
      };
      if kind.replace(parsed).is_some() {
        return Err(meta.error(
          "expected exactly one of `dictionary` or `enum`",
        ));
      }
      Ok(())
    })?;
  }
  kind.ok_or_else(|| {
    syn::Error::new(
      input.ident.span(),
      "#[derive(WebIDL)] requires #[webidl(dictionary)] or #[webidl(enum)]",
    )
  })
}

#[derive(Default)]
struct MemberAttrs {
  rename: Option<String>,
  default: Option<syn::Expr>,
  required: bool,
  options: Vec<syn::MetaNameValue>,
}

fn member_attrs(
  attrs: &[syn::Attribute],
  span: Span,
) -> Result<MemberAttrs, syn::Error> {
  let mut member = MemberAttrs::default();
  for attr in attrs {
    if attr.path().is_ident("webidl") {
      attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("rename") {
          member.rename =
            Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("default") {
          member.default = Some(meta.value()?.parse()?);
        } else if meta.path.is_ident("required") {
          member.required = true;
        } else {
          return Err(meta.error(
            "unsupported #[webidl] key on a member, expected `rename`, `default` or `required`",
          ));
        }
        Ok(())
      })?;
    } else if attr.path().is_ident("options") {
      let pairs = attr.parse_args_with(
        Punctuated::<syn::MetaNameValue, Token![,]>::parse_terminated,
      )?;
      for pair in &pairs {
        if pair.path.get_ident().is_none() {
          return Err(syn::Error::new_spanned(
            &pair.path,
            "expected an options field name",
          ));
        }
      }
      member.options.extend(pairs);
    }
  }
  if member.required && member.default.is_some() {
    return Err(syn::Error::new(
      span,
      "#[webidl(required)] cannot be combined with #[webidl(default = ...)]",
    ));
  }
  Ok(member)
}

struct Member {
  ident: syn::Ident,
  ty: syn::Type,
  key: String,
  attrs: MemberAttrs,
}

fn dictionary(
  input: &DeriveInput,
  data: &syn::DataStruct,
) -> Result<TokenStream, syn::Error> {
  let Fields::Named(fields) = &data.fields else {
    return Err(syn::Error::new(
      input.ident.span(),
      "#[webidl(dictionary)] requires a struct with named fields",
    ));
  };
  let mut members = vec![];
  let mut keys = HashSet::new();
  for field in &fields.named {
    let Some(ident) = field.ident.clone() else {
      return Err(syn::Error::new(
        field.span(),
        "#[webidl(dictionary)] requires a struct with named fields",
      ));
    };
    let attrs = member_attrs(&field.attrs, field.span())?;
    let key = attrs
      .rename
      .clone()
      .unwrap_or_else(|| stringcase::camel_case(&ident.unraw().to_string()));
    if !keys.insert(key.clone()) {
      return Err(syn::Error::new(
        field.span(),
        format!("duplicate member key '{key}'"),
      ));
    }
    members.push(Member {
      ident,
      ty: field.ty.clone(),
      key,
      attrs,
    });
  }
  // Members are read in lexicographic order of their keys, which is compared
  // by UTF-16 code units as in JavaScript.
  members.sort_by(|a, b| a.key.encode_utf16().cmp(b.key.encode_utf16()));

  let name = &input.ident;
  let dictionary_name = name.to_string();
  let reads = members
    .iter()
    .map(|member| member_read(member, &dictionary_name))
    .collect::<Vec<_>>();
  let initializers = members.iter().map(|member| {
    let ident = &member.ident;
    let binding = binding(member);
    quote!(#ident: #binding)
  });

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
        let object = deno_core::webidl::_derive::dictionary_object(
          value, &prefix, &context,
        )?;
        #(#reads)*
        let _ = object;
        ::std::result::Result::Ok(Self {
          #(#initializers,)*
        })
      }
    }
  })
}

fn binding(member: &Member) -> syn::Ident {
  format_ident!("__webidl_member_{}", member.ident)
}

/// Generates the code that reads one member, in dictionary key order.
fn member_read(member: &Member, dictionary_name: &str) -> TokenStream {
  let binding = binding(member);
  let key = &member.key;
  let ty = &member.ty;
  let options = options_tokens(member);
  // A missing member is an error unless it has a default, or it is an
  // `Option` and not `required`, in which case it is `None`.
  let missing =
    match (&member.attrs.default, member.attrs.required, is_option(ty)) {
      (Some(default), false, _) => quote!(#default),
      (None, false, true) => quote!(None),
      _ => quote! {
        return ::std::result::Result::Err(
          deno_core::webidl::_derive::required_member(&prefix, &member_context),
        )
      },
    };
  quote! {
    let #binding = {
      let member_context = || {
        deno_core::webidl::_derive::member_context(#key, #dictionary_name, &context)
      };
      let value = deno_core::webidl::_derive::dictionary_member(
        scope,
        object,
        #key,
        &prefix,
        &member_context,
      )?;
      if value.is_undefined() {
        #missing
      } else {
        #options
        <#ty as deno_core::webidl::WebIdlConverter<'a>>::convert(
          scope,
          value,
          prefix.clone(),
          &member_context,
          &member_options,
        )?
      }
    };
  }
}

/// Builds the `Options` of a member from `#[options(field = value)]`.
fn options_tokens(member: &Member) -> TokenStream {
  let ty = &member.ty;
  let fields = member.attrs.options.iter().map(|pair| &pair.path);
  let values = member.attrs.options.iter().map(|pair| &pair.value);
  if member.attrs.options.is_empty() {
    quote! {
      let member_options = <<#ty as deno_core::webidl::WebIdlConverter<'a>>::Options as ::std::default::Default>::default();
    }
  } else {
    quote! {
      #[allow(clippy::field_reassign_with_default)]
      let member_options = {
        let mut member_options = <<#ty as deno_core::webidl::WebIdlConverter<'a>>::Options as ::std::default::Default>::default();
        #(member_options.#fields = #values;)*
        member_options
      };
    }
  }
}

fn is_option(ty: &syn::Type) -> bool {
  matches!(
    ty,
    syn::Type::Path(path)
      if path
        .path
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "Option")
  )
}

fn enumeration(
  input: &DeriveInput,
  data: &syn::DataEnum,
) -> Result<TokenStream, syn::Error> {
  if data.variants.is_empty() {
    return Err(syn::Error::new(
      input.ident.span(),
      "#[webidl(enum)] requires at least one variant",
    ));
  }
  let mut arms = vec![];
  let mut values = HashSet::new();
  for variant in &data.variants {
    if !matches!(variant.fields, Fields::Unit) {
      return Err(syn::Error::new(
        variant.span(),
        "#[webidl(enum)] requires an enum with only unit variants",
      ));
    }
    let attrs = member_attrs(&variant.attrs, variant.span())?;
    if attrs.default.is_some() || attrs.required || !attrs.options.is_empty() {
      return Err(syn::Error::new(
        variant.span(),
        "only `rename` is supported on enum variants",
      ));
    }
    let value = attrs.rename.unwrap_or_else(|| {
      stringcase::kebab_case(&variant.ident.unraw().to_string())
    });
    if !values.insert(value.clone()) {
      return Err(syn::Error::new(
        variant.span(),
        format!("duplicate enum value '{value}'"),
      ));
    }
    let ident = &variant.ident;
    arms.push(quote!(#value => ::std::result::Result::Ok(Self::#ident),));
  }

  let name = &input.ident;
  let name_string = name.to_string();
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
          #(#arms)*
          _ => ::std::result::Result::Err(
            deno_core::webidl::_derive::invalid_enum_value(
              &prefix,
              &context,
              &string,
              #name_string,
            ),
          ),
        }
      }
    }
  })
}

#[cfg(test)]
mod tests {
  use pretty_assertions::assert_eq;
  use quote::ToTokens;
  use std::path::PathBuf;

  use super::*;

  fn derives_webidl(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
      attr.path().is_ident("derive")
        && attr.to_token_stream().to_string().contains("WebIDL")
    })
  }

  #[testing_macros::fixture("webidl/test_cases/*.rs")]
  fn test_derive_output(input: PathBuf) {
    let update_expected = std::env::var("UPDATE_EXPECTED").is_ok();

    let source =
      std::fs::read_to_string(&input).expect("Failed to read test file");

    const PRELUDE: &str = r"// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();";

    if !source.starts_with(PRELUDE) {
      panic!("Source does not start with expected prelude:\n{PRELUDE}");
    }

    let file =
      syn::parse_str::<syn::File>(&source).expect("Failed to parse Rust file");
    let mut expected_out = vec![];
    for item in file.items {
      let attrs = match &item {
        syn::Item::Struct(item) => &item.attrs,
        syn::Item::Enum(item) => &item.attrs,
        _ => continue,
      };
      if !derives_webidl(attrs) {
        continue;
      }
      let tokens = webidl(item.to_token_stream())
        .expect("Failed to expand #[derive(WebIDL)]");
      let tree: syn::File =
        syn::parse2(tokens).expect("Failed to parse expansion");
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

  fn expand_error(source: &str) -> String {
    webidl(source.parse().unwrap()).unwrap_err().to_string()
  }

  #[test]
  fn test_derive_errors() {
    assert_eq!(
      expand_error("struct Foo {}"),
      "#[derive(WebIDL)] requires #[webidl(dictionary)] or #[webidl(enum)]"
    );
    assert_eq!(
      expand_error("#[webidl(bogus)] struct Foo {}"),
      "unsupported #[webidl] key on a type, expected `dictionary` or `enum`"
    );
    assert_eq!(
      expand_error("#[webidl(dictionary)] struct Foo(u8);"),
      "#[webidl(dictionary)] requires a struct with named fields"
    );
    assert_eq!(
      expand_error("#[webidl(dictionary)] struct Foo;"),
      "#[webidl(dictionary)] requires a struct with named fields"
    );
    assert_eq!(
      expand_error("#[webidl(dictionary)] enum Foo { A }"),
      "#[webidl(dictionary)] can only be used on a struct"
    );
    assert_eq!(
      expand_error("#[webidl(enum)] struct Foo { a: u8 }"),
      "#[webidl(enum)] can only be used on an enum"
    );
    assert_eq!(
      expand_error("#[webidl(enum)] enum Foo { A(u8) }"),
      "#[webidl(enum)] requires an enum with only unit variants"
    );
    assert_eq!(
      expand_error("#[webidl(enum)] enum Foo {}"),
      "#[webidl(enum)] requires at least one variant"
    );
    assert_eq!(
      expand_error(
        "#[webidl(enum)] enum Foo { A, #[webidl(rename = \"a\")] B }"
      ),
      "duplicate enum value 'a'"
    );
    assert_eq!(
      expand_error("#[webidl(enum)] enum Foo { #[webidl(default = 1)] A }"),
      "only `rename` is supported on enum variants"
    );
    assert_eq!(
      expand_error(
        "#[webidl(dictionary)] struct Foo { #[webidl(bogus)] a: u8 }"
      ),
      "unsupported #[webidl] key on a member, expected `rename`, `default` or `required`"
    );
    assert_eq!(
      expand_error(
        "#[webidl(dictionary)] struct Foo { #[webidl(required, default = 1)] a: u8 }"
      ),
      "#[webidl(required)] cannot be combined with #[webidl(default = ...)]"
    );
    assert_eq!(
      expand_error(
        "#[webidl(dictionary)] struct Foo { #[options(a::b = 1)] a: u8 }"
      ),
      "expected an options field name"
    );
    assert_eq!(
      expand_error(
        "#[webidl(dictionary)] struct Foo { a_b: u8, #[webidl(rename = \"aB\")] a: u8 }"
      ),
      "duplicate member key 'aB'"
    );
    assert_eq!(
      expand_error("#[webidl(dictionary)] struct Foo<T> { a: T }"),
      "#[derive(WebIDL)] does not support generic types"
    );
  }
}
