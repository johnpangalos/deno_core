// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! Expansion of `#[derive(WebIDL)]`. The derive is documented on the
//! crate-level `WebIDL` macro in `ops/lib.rs`.

use proc_macro2::TokenStream;
use quote::format_ident;
use quote::quote;
use syn::ext::IdentExt;
use syn::parse_quote;
use syn::spanned::Spanned;
use syn::Attribute;
use syn::Data;
use syn::DeriveInput;
use syn::Expr;
use syn::Fields;
use syn::Ident;
use syn::LitStr;
use syn::Type;
use syn::Variant;

const OPTIONS_MSG: &str = "#[options] is only valid on dictionary members";

/// A dictionary member, read from its `#[webidl]` and `#[options]` attributes.
struct Member {
  ident: Ident,
  ty: Type,
  key: String,
  required: bool,
  default: Option<Expr>,
  options: Vec<(Ident, Expr)>,
}

impl Member {
  fn parse(field: &syn::Field) -> syn::Result<Self> {
    let Some(ident) = field.ident.clone() else {
      return Err(syn::Error::new(
        field.span(),
        "#[webidl(dictionary)] requires named fields",
      ));
    };
    let mut rename = None;
    let mut required = false;
    let mut default = None;
    let mut options = vec![];
    for attr in &field.attrs {
      if attr.path().is_ident("webidl") {
        attr.parse_nested_meta(|meta| {
          if meta.path.is_ident("rename") {
            rename = Some(meta.value()?.parse::<LitStr>()?.value());
          } else if meta.path.is_ident("required") {
            required = true;
          } else if meta.path.is_ident("default") {
            default = Some(meta.value()?.parse::<Expr>()?);
          } else {
            return Err(meta.error(
              "unknown field attribute, expected `rename`, `required` or `default`",
            ));
          }
          Ok(())
        })?;
      } else if attr.path().is_ident("options") {
        attr.parse_nested_meta(|meta| {
          let Some(name) = meta.path.get_ident().cloned() else {
            return Err(meta.error("expected the name of an options field"));
          };
          let value = meta.value()?.parse::<Expr>()?;
          options.push((name, value));
          Ok(())
        })?;
      }
    }
    if required && default.is_some() {
      return Err(syn::Error::new(
        ident.span(),
        "`required` and `default` can not be used together",
      ));
    }
    let key = rename
      .unwrap_or_else(|| stringcase::camel_case(&ident.unraw().to_string()));
    Ok(Self {
      ident,
      ty: field.ty.clone(),
      key,
      required,
      default,
      options,
    })
  }
}

/// Expands `#[derive(WebIDL)]` for one type.
pub(crate) fn derive(input: &DeriveInput) -> syn::Result<TokenStream> {
  reject_options(&input.attrs)?;
  match container_kind(input)? {
    Kind::Dictionary => dictionary(input),
    Kind::Enum => enumeration(input),
  }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
  Dictionary,
  Enum,
}

fn container_kind(input: &DeriveInput) -> syn::Result<Kind> {
  let mut kind = None;
  for attr in input.attrs.iter().filter(|a| a.path().is_ident("webidl")) {
    attr.parse_nested_meta(|meta| {
      let parsed = if meta.path.is_ident("dictionary") {
        Kind::Dictionary
      } else if meta.path.is_ident("enum") {
        Kind::Enum
      } else {
        return Err(
          meta.error("unknown kind, expected `dictionary` or `enum`"),
        );
      };
      if kind.replace(parsed).is_some() {
        return Err(meta.error("the kind is already set"));
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

fn reject_options(attrs: &[Attribute]) -> syn::Result<()> {
  match attrs.iter().find(|a| a.path().is_ident("options")) {
    Some(attr) => Err(syn::Error::new(attr.span(), OPTIONS_MSG)),
    None => Ok(()),
  }
}

/// Wraps the body of `convert` in the `WebIdlConverter` impl. The impl gets an
/// extra `'a` lifetime, which the body uses for the V8 scope.
fn implementation(input: &DeriveInput, body: TokenStream) -> TokenStream {
  let name = &input.ident;
  let mut generics = input.generics.clone();
  generics.params.insert(0, parse_quote!('a));
  let (impl_generics, _, _) = generics.split_for_impl();
  let (_, ty_generics, where_clause) = input.generics.split_for_impl();
  quote! {
    impl #impl_generics ::deno_core::webidl::WebIdlConverter<'a> for #name #ty_generics #where_clause {
      type Options = ();

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

fn dictionary(input: &DeriveInput) -> syn::Result<TokenStream> {
  let Data::Struct(data) = &input.data else {
    return Err(syn::Error::new(
      input.ident.span(),
      "#[webidl(dictionary)] can only be used on structs",
    ));
  };
  let Fields::Named(fields) = &data.fields else {
    return Err(syn::Error::new(
      input.ident.span(),
      "#[webidl(dictionary)] requires a struct with named fields",
    ));
  };
  let members = fields
    .named
    .iter()
    .map(Member::parse)
    .collect::<syn::Result<Vec<_>>>()?;

  let mut keys = std::collections::HashSet::new();
  for member in &members {
    if !keys.insert(member.key.as_str()) {
      return Err(syn::Error::new(
        member.ident.span(),
        format!("duplicate member key '{}'", member.key),
      ));
    }
  }

  // Members are read in the code-unit order of their keys, regardless of the
  // order they are declared in.
  let mut order: Vec<usize> = (0..members.len()).collect();
  order.sort_by(|&a, &b| {
    members[a]
      .key
      .encode_utf16()
      .cmp(members[b].key.encode_utf16())
  });
  let dictionary = input.ident.to_string();
  let reads = order
    .iter()
    .map(|&index| read_member(&members[index], index, &dictionary));
  let initializers = members.iter().enumerate().map(|(index, member)| {
    let ident = &member.ident;
    let tmp = member_ident(index);
    quote!(#ident: #tmp)
  });

  let body = if members.is_empty() {
    quote! {
      let _ = scope;
      ::deno_core::webidl::dictionary_object(value, &prefix, &context)?;
      ::std::result::Result::Ok(Self {})
    }
  } else {
    quote! {
      let object = ::deno_core::webidl::dictionary_object(value, &prefix, &context)?;
      #(#reads)*
      ::std::result::Result::Ok(Self { #(#initializers),* })
    }
  };
  Ok(implementation(input, body))
}

fn member_ident(index: usize) -> Ident {
  format_ident!("__member_{}", index)
}

/// Generates the code that reads and converts one member into a local.
fn read_member(member: &Member, index: usize, dictionary: &str) -> TokenStream {
  let ty = &member.ty;
  let key = &member.key;
  let tmp = member_ident(index);
  // Struct literals need a plain path, so the options are set field by field.
  let options = if member.options.is_empty() {
    quote!(::std::default::Default::default())
  } else {
    let fields = member
      .options
      .iter()
      .map(|(name, value)| quote!(options.#name = #value;));
    quote! {
      {
        let mut options =
          <#ty as ::deno_core::webidl::WebIdlConverter<'a>>::Options::default();
        #(#fields)*
        options
      }
    }
  };
  let required_error = quote! {
    return ::std::result::Result::Err(::deno_core::webidl::WebIdlError::new(
      prefix.clone(),
      &context(),
      ::deno_core::webidl::WebIdlErrorKind::Required {
        dictionary: #dictionary,
        key: #key.to_string(),
      },
    ))
  };
  let missing = if member.required {
    required_error
  } else if let Some(default) = &member.default {
    quote!(#default)
  } else if is_option(ty) {
    quote!(::std::option::Option::None)
  } else {
    required_error
  };
  quote! {
    let #tmp: #ty = match ::deno_core::webidl::dictionary_member(
      scope,
      object,
      #key,
      #dictionary,
      &prefix,
      &context,
    )? {
      ::std::option::Option::Some(value) => {
        <#ty as ::deno_core::webidl::WebIdlConverter<'a>>::convert(
          scope,
          value,
          prefix.clone(),
          || ::std::borrow::Cow::Owned(
            ::deno_core::webidl::dictionary_member_context(
              #key,
              #dictionary,
              &context(),
            ),
          ),
          &#options,
        )?
      }
      ::std::option::Option::None => #missing,
    };
  }
}

fn is_option(ty: &Type) -> bool {
  match ty {
    Type::Path(path) => path
      .path
      .segments
      .last()
      .is_some_and(|segment| segment.ident == "Option"),
    _ => false,
  }
}

fn enumeration(input: &DeriveInput) -> syn::Result<TokenStream> {
  let Data::Enum(data) = &input.data else {
    return Err(syn::Error::new(
      input.ident.span(),
      "#[webidl(enum)] can only be used on enums",
    ));
  };
  if data.variants.is_empty() {
    return Err(syn::Error::new(
      input.ident.span(),
      "#[webidl(enum)] requires at least one variant",
    ));
  }
  let mut values = std::collections::HashSet::new();
  let mut arms = vec![];
  for variant in &data.variants {
    let value = variant_value(variant)?;
    if !values.insert(value.clone()) {
      return Err(syn::Error::new(
        variant.ident.span(),
        format!("duplicate enum value '{value}'"),
      ));
    }
    let ident = &variant.ident;
    arms.push(quote!(#value => ::std::option::Option::Some(Self::#ident)));
  }
  let name = input.ident.to_string();
  let body = quote! {
    let received = ::deno_core::webidl::enum_string(scope, value, &prefix, &context)?;
    let variant = match received.as_str() {
      #(#arms,)*
      _ => ::std::option::Option::None,
    };
    variant.ok_or_else(|| ::deno_core::webidl::WebIdlError::new(
      prefix.clone(),
      &context(),
      ::deno_core::webidl::WebIdlErrorKind::InvalidEnumValue {
        value: received,
        enum_name: #name,
      },
    ))
  };
  Ok(implementation(input, body))
}

/// The string a unit variant is matched against: its `rename`, or its name in
/// kebab-case.
fn variant_value(variant: &Variant) -> syn::Result<String> {
  if !matches!(variant.fields, Fields::Unit) {
    return Err(syn::Error::new(
      variant.span(),
      "#[webidl(enum)] variants must be unit variants",
    ));
  }
  let mut rename = None;
  for attr in &variant.attrs {
    if attr.path().is_ident("webidl") {
      attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("rename") {
          rename = Some(meta.value()?.parse::<LitStr>()?.value());
          Ok(())
        } else {
          Err(meta.error("unknown variant attribute, expected `rename`"))
        }
      })?;
    } else if attr.path().is_ident("options") {
      return Err(syn::Error::new(attr.span(), OPTIONS_MSG));
    }
  }
  Ok(rename.unwrap_or_else(|| {
    stringcase::kebab_case(&variant.ident.unraw().to_string())
  }))
}

#[cfg(test)]
mod tests {
  use super::*;
  use pretty_assertions::assert_eq;
  use std::path::PathBuf;
  use syn::Item;

  #[testing_macros::fixture("webidl/test_cases/*.rs")]
  fn test_proc_macro_derive(input: PathBuf) {
    let update_expected = std::env::var("UPDATE_EXPECTED").is_ok();
    let source =
      std::fs::read_to_string(&input).expect("Failed to read test file");
    let file =
      syn::parse_str::<syn::File>(&source).expect("Failed to parse Rust file");

    let mut expected_out = vec![];
    for item in file.items {
      let derive_input = match item {
        Item::Struct(item) => DeriveInput::from(item),
        Item::Enum(item) => DeriveInput::from(item),
        _ => continue,
      };
      let tokens = derive(&derive_input).expect("Failed to expand derive");
      println!("======== Raw tokens ========:\n{}", tokens.clone());
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

  fn expand_error(source: &str) -> String {
    let input = syn::parse_str::<DeriveInput>(source).unwrap();
    derive(&input).expect_err("expected an error").to_string()
  }

  #[test]
  fn rejects_bad_shapes_with_exact_messages() {
    let cases = [
      (
        "struct S { a: u8 }",
        "#[derive(WebIDL)] requires #[webidl(dictionary)] or #[webidl(enum)]",
      ),
      (
        "#[webidl(other)] struct S { a: u8 }",
        "unknown kind, expected `dictionary` or `enum`",
      ),
      (
        "#[webidl(dictionary, enum)] struct S { a: u8 }",
        "the kind is already set",
      ),
      (
        "#[webidl(dictionary)] struct S(u8);",
        "#[webidl(dictionary)] requires a struct with named fields",
      ),
      (
        "#[webidl(dictionary)] struct S;",
        "#[webidl(dictionary)] requires a struct with named fields",
      ),
      (
        "#[webidl(dictionary)] enum E { A }",
        "#[webidl(dictionary)] can only be used on structs",
      ),
      (
        "#[webidl(enum)] struct S { a: u8 }",
        "#[webidl(enum)] can only be used on enums",
      ),
      (
        "#[webidl(enum)] enum E {}",
        "#[webidl(enum)] requires at least one variant",
      ),
      (
        "#[webidl(enum)] enum E { A(u8) }",
        "#[webidl(enum)] variants must be unit variants",
      ),
      (
        "#[webidl(enum)] enum E { #[webidl(default = 1)] A }",
        "unknown variant attribute, expected `rename`",
      ),
      (
        "#[webidl(enum)] enum E { A, #[webidl(rename = \"a\")] B }",
        "duplicate enum value 'a'",
      ),
      (
        "#[webidl(dictionary)] struct S { #[webidl(bogus)] a: u8 }",
        "unknown field attribute, expected `rename`, `required` or `default`",
      ),
      (
        "#[webidl(dictionary)] struct S { #[webidl(required, default = 1)] a: u8 }",
        "`required` and `default` can not be used together",
      ),
      (
        "#[webidl(dictionary)] struct S { a: u8, #[webidl(rename = \"a\")] b: u8 }",
        "duplicate member key 'a'",
      ),
      (
        "#[webidl(dictionary)] #[options(clamp = true)] struct S { a: u8 }",
        "#[options] is only valid on dictionary members",
      ),
      (
        "#[webidl(enum)] enum E { #[options(clamp = true)] A }",
        "#[options] is only valid on dictionary members",
      ),
    ];
    for (source, message) in cases {
      assert_eq!(expand_error(source), message, "{source}");
    }
  }
}
