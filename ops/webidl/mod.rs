// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! Implementation of `#[derive(WebIDL)]`, which generates a
//! `deno_core::webidl::WebIdlConverter` for dictionaries (structs with named
//! fields) and enums of unit variants.

use proc_macro2::Span;
use proc_macro2::TokenStream;
use quote::format_ident;
use quote::quote;
use std::collections::HashSet;
use syn::ext::IdentExt;
use syn::parse::Parse;
use syn::parse::ParseStream;
use syn::punctuated::Punctuated;
use syn::Attribute;
use syn::Data;
use syn::DataEnum;
use syn::DeriveInput;
use syn::Expr;
use syn::ExprLit;
use syn::Fields;
use syn::FieldsNamed;
use syn::Ident;
use syn::Lit;
use syn::Token;
use syn::Type;

enum Kind {
  Dictionary,
  Enum,
}

/// One `key` or `key = value` item inside `#[webidl(...)]` or `#[options(...)]`.
struct Arg {
  key: Ident,
  value: Option<Expr>,
}

impl Parse for Arg {
  fn parse(input: ParseStream) -> syn::Result<Self> {
    // `parse_any` accepts keywords, such as `enum`, that `syn::Meta` rejects.
    let key = Ident::parse_any(input)?;
    let value = if input.peek(Token![=]) {
      input.parse::<Token![=]>()?;
      Some(input.parse::<Expr>()?)
    } else {
      None
    };
    Ok(Self { key, value })
  }
}

/// The `#[webidl(...)]` and `#[options(...)]` attributes of a field or variant.
#[derive(Default)]
struct MemberAttrs {
  rename: Option<String>,
  default: Option<Expr>,
  required: bool,
  /// Converter option fields, and the values they are set to.
  options: Vec<(Ident, Expr)>,
}

struct Member {
  ident: Ident,
  key: String,
  ty: Type,
  attrs: MemberAttrs,
}

pub fn derive(input: TokenStream) -> syn::Result<TokenStream> {
  let input: DeriveInput = syn::parse2(input)?;
  if !input.generics.params.is_empty() {
    return Err(syn::Error::new_spanned(
      &input.generics,
      "#[derive(WebIDL)] does not support generic types",
    ));
  }
  let name = &input.ident;
  let kind = container_kind(&input.attrs)?;
  let body = match (&input.data, kind) {
    (Data::Struct(data), Kind::Dictionary) => match &data.fields {
      Fields::Named(fields) => dictionary_body(name, fields)?,
      _ => {
        return Err(syn::Error::new_spanned(
          name,
          "#[webidl(dictionary)] requires a struct with named fields",
        ))
      }
    },
    (Data::Enum(data), Kind::Enum) => enum_body(name, data)?,
    (Data::Struct(_), Kind::Enum) => {
      return Err(syn::Error::new_spanned(
        name,
        "#[webidl(enum)] can only be used on enums",
      ))
    }
    (Data::Enum(_), Kind::Dictionary) => {
      return Err(syn::Error::new_spanned(
        name,
        "#[webidl(dictionary)] can only be used on structs",
      ))
    }
    (Data::Union(_), _) => {
      return Err(syn::Error::new_spanned(
        name,
        "#[derive(WebIDL)] does not support unions",
      ))
    }
  };

  Ok(quote! {
    impl<'a> deno_core::webidl::WebIdlConverter<'a> for #name {
      type Options = ();

      fn convert<C>(
        scope: &mut deno_core::v8::HandleScope<'a>,
        value: deno_core::v8::Local<'a, deno_core::v8::Value>,
        prefix: ::std::borrow::Cow<'static, str>,
        context: C,
        _options: &Self::Options,
      ) -> ::std::result::Result<Self, deno_core::webidl::WebIdlError>
      where
        C: Fn() -> ::std::borrow::Cow<'static, str>,
      {
        #body
      }
    }
  })
}

fn parse_args(attr: &Attribute) -> syn::Result<Vec<Arg>> {
  let args =
    attr.parse_args_with(Punctuated::<Arg, Token![,]>::parse_terminated)?;
  Ok(args.into_iter().collect())
}

/// Parses the items of every `#[webidl(...)]` attribute.
fn webidl_args(attrs: &[Attribute]) -> syn::Result<Vec<Arg>> {
  let mut args = vec![];
  for attr in attrs.iter().filter(|attr| attr.path().is_ident("webidl")) {
    args.extend(parse_args(attr)?);
  }
  Ok(args)
}

fn unknown_key(arg: &Arg) -> syn::Error {
  syn::Error::new_spanned(
    &arg.key,
    format!("unknown #[webidl] key '{}'", arg.key),
  )
}

fn container_kind(attrs: &[Attribute]) -> syn::Result<Kind> {
  let mut kind = None;
  for arg in webidl_args(attrs)? {
    let this = match (arg.key.to_string().as_str(), arg.value.is_some()) {
      ("dictionary", false) => Kind::Dictionary,
      ("enum", false) => Kind::Enum,
      _ => return Err(unknown_key(&arg)),
    };
    if kind.is_some() {
      return Err(syn::Error::new_spanned(
        &arg.key,
        "use only one of #[webidl(dictionary)] or #[webidl(enum)]",
      ));
    }
    kind = Some(this);
  }
  kind.ok_or_else(|| {
    syn::Error::new(
      Span::call_site(),
      "missing #[webidl(dictionary)] or #[webidl(enum)]",
    )
  })
}

fn string_literal(expr: &Expr) -> syn::Result<String> {
  match expr {
    Expr::Lit(ExprLit {
      lit: Lit::Str(string),
      ..
    }) => Ok(string.value()),
    _ => Err(syn::Error::new_spanned(expr, "expected a string literal")),
  }
}

fn member_attrs(attrs: &[Attribute]) -> syn::Result<MemberAttrs> {
  let mut member = MemberAttrs::default();
  for arg in webidl_args(attrs)? {
    let name = arg.key.to_string();
    match (name.as_str(), &arg.value) {
      ("rename", Some(value)) => member.rename = Some(string_literal(value)?),
      ("default", Some(value)) => member.default = Some(value.clone()),
      ("required", None) => member.required = true,
      _ => return Err(unknown_key(&arg)),
    }
  }
  if member.required && member.default.is_some() {
    return Err(syn::Error::new(
      Span::call_site(),
      "#[webidl(required)] can not be combined with #[webidl(default)]",
    ));
  }
  for attr in attrs.iter().filter(|attr| attr.path().is_ident("options")) {
    for arg in parse_args(attr)? {
      let Some(value) = arg.value else {
        return Err(syn::Error::new_spanned(
          &arg.key,
          "expected an option value, like `clamp = true`",
        ));
      };
      member.options.push((arg.key, value));
    }
  }
  Ok(member)
}

/// Whether `ty` is `Option<T>`, which is `None` when the member is missing.
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

fn dictionary_body(
  name: &Ident,
  fields: &FieldsNamed,
) -> syn::Result<TokenStream> {
  let mut members = vec![];
  for field in &fields.named {
    let ident = field.ident.clone().expect("named fields have identifiers");
    let attrs = member_attrs(&field.attrs)?;
    let key = attrs
      .rename
      .clone()
      .unwrap_or_else(|| stringcase::camel_case(&ident.unraw().to_string()));
    members.push(Member {
      ident,
      key,
      ty: field.ty.clone(),
      attrs,
    });
  }
  // Members are read, and converted, in lexicographic order of their keys,
  // as the WebIDL dictionary algorithm requires.
  members.sort_by(|a, b| a.key.cmp(&b.key));
  for pair in members.windows(2) {
    if pair[0].key == pair[1].key {
      return Err(syn::Error::new_spanned(
        &pair[1].ident,
        format!("duplicate dictionary member '{}'", pair[1].key),
      ));
    }
  }

  let name_str = name.to_string();
  // Each member is read with `Get` and converted before the next member is read,
  // in lexicographic key order, as the WebIDL dictionary algorithm requires.
  // The generated locals are numbered rather than named after the fields, so
  // that no field name can collide with the names the generated code uses.
  let locals: Vec<Ident> = (0..members.len())
    .map(|index| format_ident!("__webidl_member_{index}"))
    .collect();
  let convert = members.iter().zip(&locals).map(|(member, local)| {
    let ty = &member.ty;
    let key = &member.key;
    let option_sets = member.attrs.options.iter().map(|(option, value)| {
      quote!(options.#option = #value;)
    });
    let missing = if let Some(default) = &member.attrs.default {
      quote!(#default)
    } else if is_option(ty) && !member.attrs.required {
      quote!(None)
    } else {
      quote! {
        return Err(deno_core::_ops::required_member_error(
          &prefix, &context, #key, #name_str,
        ))
      }
    };
    quote! {
      let #local: #ty = {
        let raw = deno_core::_ops::dictionary_member(
          scope, dictionary, #key, &prefix, &context,
        )?;
        match raw {
          Some(value) => {
            let options = {
              #[allow(unused_mut)]
              let mut options = <<#ty as deno_core::webidl::WebIdlConverter<'a>>::Options as Default>::default();
              #(#option_sets)*
              options
            };
            <#ty as deno_core::webidl::WebIdlConverter<'a>>::convert(
              scope,
              value,
              prefix.clone(),
              || ::std::borrow::Cow::Owned(format!("{}, member '{}'", context(), #key)),
              &options,
            )?
          }
          None => #missing,
        }
      };
    }
  });

  let unused = if members.is_empty() {
    quote!(let _ = (&scope, dictionary);)
  } else {
    quote!()
  };
  let fields: Vec<&Ident> =
    members.iter().map(|member| &member.ident).collect();
  Ok(quote! {
    let dictionary = deno_core::_ops::dictionary_object(
      value, &prefix, &context, #name_str,
    )?;
    #unused
    #(#convert)*
    Ok(Self { #(#fields: #locals),* })
  })
}

fn enum_body(name: &Ident, data: &DataEnum) -> syn::Result<TokenStream> {
  if data.variants.is_empty() {
    return Err(syn::Error::new_spanned(
      name,
      "#[webidl(enum)] requires at least one variant",
    ));
  }
  let mut arms = vec![];
  let mut seen = HashSet::new();
  for variant in &data.variants {
    if !matches!(variant.fields, Fields::Unit) {
      return Err(syn::Error::new_spanned(
        variant,
        "#[webidl(enum)] only supports unit variants",
      ));
    }
    let attrs = member_attrs(&variant.attrs)?;
    if attrs.default.is_some() || attrs.required || !attrs.options.is_empty() {
      return Err(syn::Error::new_spanned(
        variant,
        "only #[webidl(rename = \"...\")] is valid on enum variants",
      ));
    }
    let js_value = attrs.rename.unwrap_or_else(|| {
      stringcase::kebab_case(&variant.ident.unraw().to_string())
    });
    if !seen.insert(js_value.clone()) {
      return Err(syn::Error::new_spanned(
        variant,
        format!("duplicate enum value '{js_value}'"),
      ));
    }
    let ident = &variant.ident;
    arms.push(quote!(#js_value => Ok(Self::#ident),));
  }

  let name_str = name.to_string();
  Ok(quote! {
    let string = <::std::string::String as deno_core::webidl::WebIdlConverter<'a>>::convert(
      scope,
      value,
      prefix.clone(),
      &context,
      &Default::default(),
    )?;
    match string.as_str() {
      #(#arms)*
      _ => Err(deno_core::webidl::WebIdlError::new(
        prefix,
        context(),
        format!("is not a valid value of enum '{}': '{}'.", #name_str, string),
      )),
    }
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use quote::ToTokens;
  use std::path::PathBuf;

  fn expand(source: &str) -> Result<String, String> {
    let input = syn::parse_str::<TokenStream>(source).unwrap();
    let tokens = derive(input).map_err(|err| err.to_string())?;
    let tree = syn::parse2(tokens).unwrap();
    Ok(prettyplease::unparse(&tree))
  }

  fn expand_error(source: &str) -> String {
    expand(source).expect_err("expected the derive to fail")
  }

  #[test]
  fn rejects_tuple_struct() {
    assert_eq!(
      expand_error("#[webidl(dictionary)] struct Foo(u8);"),
      "#[webidl(dictionary)] requires a struct with named fields"
    );
  }

  #[test]
  fn rejects_generics() {
    assert_eq!(
      expand_error("#[webidl(dictionary)] struct Foo<T> { a: T }"),
      "#[derive(WebIDL)] does not support generic types"
    );
  }

  #[test]
  fn rejects_missing_kind() {
    assert_eq!(
      expand_error("struct Foo { a: u8 }"),
      "missing #[webidl(dictionary)] or #[webidl(enum)]"
    );
  }

  #[test]
  fn rejects_required_with_default() {
    assert_eq!(
      expand_error(
        "#[webidl(dictionary)] struct Foo { #[webidl(required, default = 1)] a: u8 }"
      ),
      "#[webidl(required)] can not be combined with #[webidl(default)]"
    );
  }

  #[test]
  fn rejects_unknown_key() {
    assert_eq!(
      expand_error(
        "#[webidl(dictionary)] struct Foo { #[webidl(bogus)] a: u8 }"
      ),
      "unknown #[webidl] key 'bogus'"
    );
  }

  #[test]
  fn rejects_enum_with_data() {
    assert_eq!(
      expand_error("#[webidl(enum)] enum Foo { A(u8) }"),
      "#[webidl(enum)] only supports unit variants"
    );
  }

  #[test]
  fn rejects_duplicate_member_keys() {
    assert_eq!(
      expand_error(
        "#[webidl(dictionary)] struct Foo { a_b: u8, #[webidl(rename = \"aB\")] c: u8 }"
      ),
      "duplicate dictionary member 'aB'"
    );
  }

  #[test]
  fn rejects_dictionary_on_enum() {
    assert_eq!(
      expand_error("#[webidl(dictionary)] enum Foo { A }"),
      "#[webidl(dictionary)] can only be used on structs"
    );
  }

  #[testing_macros::fixture("webidl/test_cases/*.rs")]
  fn test_webidl_derive(input: PathBuf) {
    let update_expected = std::env::var("UPDATE_EXPECTED").is_ok();
    let source =
      std::fs::read_to_string(&input).expect("Failed to read test file");
    const HEADER: &str = "// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.\n#![deny(warnings)]\n";
    if !source.starts_with(HEADER) {
      panic!("Source does not start with expected header:\n{HEADER}");
    }

    let file =
      syn::parse_str::<syn::File>(&source).expect("Failed to parse Rust file");
    let mut expected_out = vec![];
    for mut item in file.items {
      let attrs = match &mut item {
        syn::Item::Struct(item) => &mut item.attrs,
        syn::Item::Enum(item) => &mut item.attrs,
        _ => continue,
      };
      let Some(position) = attrs.iter().position(|attr| {
        attr.path().is_ident("derive")
          && attr.to_token_stream().to_string().contains("WebIDL")
      }) else {
        continue;
      };
      attrs.remove(position);
      let tokens = derive(item.to_token_stream()).expect("Failed to derive");
      let tree = syn::parse2(tokens).expect("Failed to parse expansion");
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
}
