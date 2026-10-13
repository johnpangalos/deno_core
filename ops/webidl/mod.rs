// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! `#[derive(WebIDL)]`: generates `deno_core::webidl::WebIdlConverter`
//! implementations for dictionaries (`#[webidl(dictionary)]` on a struct with
//! named fields) and enums of unit variants (`#[webidl(enum)]`).

use proc_macro2::TokenStream;
use quote::format_ident;
use quote::quote;
use syn::ext::IdentExt;
use syn::spanned::Spanned;
use syn::Attribute;
use syn::Data;
use syn::DeriveInput;
use syn::Expr;
use syn::Fields;
use syn::Ident;
use syn::LitStr;
use syn::Type;

#[derive(Default)]
struct ContainerAttrs {
  dictionary: bool,
  enumeration: bool,
}

#[derive(Default)]
struct MemberAttrs {
  rename: Option<String>,
  default: Option<Expr>,
  required: bool,
  options: Vec<(Ident, Expr)>,
}

fn container_attrs(attrs: &[Attribute]) -> syn::Result<ContainerAttrs> {
  let mut out = ContainerAttrs::default();
  for attr in attrs.iter().filter(|a| a.path().is_ident("webidl")) {
    attr.parse_nested_meta(|meta| {
      if meta.path.is_ident("dictionary") {
        out.dictionary = true;
        Ok(())
      } else if meta.path.is_ident("enum") {
        out.enumeration = true;
        Ok(())
      } else {
        Err(meta.error("unknown webidl attribute"))
      }
    })?;
  }
  Ok(out)
}

fn member_attrs(attrs: &[Attribute]) -> syn::Result<MemberAttrs> {
  let mut out = MemberAttrs::default();
  for attr in attrs.iter().filter(|a| a.path().is_ident("webidl")) {
    attr.parse_nested_meta(|meta| {
      if meta.path.is_ident("rename") {
        let value: LitStr = meta.value()?.parse()?;
        out.rename = Some(value.value());
        Ok(())
      } else if meta.path.is_ident("default") {
        out.default = Some(meta.value()?.parse()?);
        Ok(())
      } else if meta.path.is_ident("required") {
        out.required = true;
        Ok(())
      } else {
        Err(meta.error("unknown webidl attribute"))
      }
    })?;
  }
  if out.required && out.default.is_some() {
    return Err(syn::Error::new(
      attrs[0].span(),
      "a member can not be both `required` and have a `default`",
    ));
  }
  for attr in attrs.iter().filter(|a| a.path().is_ident("options")) {
    attr.parse_nested_meta(|meta| {
      let Some(key) = meta.path.get_ident().cloned() else {
        return Err(meta.error("expected an option name"));
      };
      let value: Expr = meta.value()?.parse()?;
      out.options.push((key, value));
      Ok(())
    })?;
  }
  Ok(out)
}

/// Returns true if `ty` is written as `Option<...>`.
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

pub(crate) fn derive_webidl(input: DeriveInput) -> syn::Result<TokenStream> {
  if !input.generics.params.is_empty() {
    return Err(syn::Error::new(
      input.generics.span(),
      "#[derive(WebIDL)] does not support generic types",
    ));
  }
  let container = container_attrs(&input.attrs)?;
  if container.dictionary && container.enumeration {
    return Err(syn::Error::new(
      input.ident.span(),
      "a type can not be both `dictionary` and `enum`",
    ));
  }
  match (&input.data, container) {
    (Data::Struct(data), ContainerAttrs { dictionary: true, .. }) => {
      let Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new(
          input.ident.span(),
          "#[webidl(dictionary)] requires a struct with named fields",
        ));
      };
      dictionary(&input.ident, fields)
    }
    (Data::Enum(data), ContainerAttrs { enumeration: true, .. }) => {
      enumeration(&input.ident, data)
    }
    _ => Err(syn::Error::new(
      input.ident.span(),
      "#[derive(WebIDL)] requires `#[webidl(dictionary)]` on a struct or `#[webidl(enum)]` on an enum",
    )),
  }
}

fn dictionary(
  ident: &Ident,
  fields: &syn::FieldsNamed,
) -> syn::Result<TokenStream> {
  let name = ident.to_string();

  // Each member is read in the lexicographic (code unit) order of its key,
  // regardless of the order of the fields in the struct.
  let mut members = vec![];
  for field in &fields.named {
    let field_ident = field.ident.clone().expect("named field");
    let attrs = member_attrs(&field.attrs)?;
    let key = attrs.rename.clone().unwrap_or_else(|| {
      stringcase::camel_case(&field_ident.unraw().to_string())
    });
    members.push((key, field_ident, field.ty.clone(), attrs));
  }
  members.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));

  let mut statements = TokenStream::new();
  let mut assignments = TokenStream::new();
  for (key, field_ident, ty, attrs) in &members {
    let var = format_ident!("__webidl_{}", field_ident);
    let option_setters = attrs.options.iter().map(|(key, value)| {
      quote! { __options.#key = #value; }
    });
    let options = if attrs.options.is_empty() {
      quote! { &Default::default() }
    } else {
      quote! {
        &{
          let mut __options: <#ty as deno_core::webidl::WebIdlConverter<'a>>::Options = Default::default();
          #(#option_setters)*
          __options
        }
      }
    };
    let missing = if let Some(default) = &attrs.default {
      quote! { #default }
    } else if !attrs.required && is_option(ty) {
      quote! { None }
    } else {
      quote! {
        return Err(deno_core::webidl::missing_dictionary_member(
          &prefix, &context, #name, #key,
        ))
      }
    };
    statements.extend(quote! {
      let #var = match deno_core::webidl::dictionary_member(
        scope, __dictionary, #key, &prefix, &context,
      )? {
        Some(__value) => {
          let __member_context = || {
            std::borrow::Cow::Owned(format!(
              "'{}' of '{}' ({})",
              #key,
              #name,
              context(),
            ))
          };
          <#ty as deno_core::webidl::WebIdlConverter<'a>>::convert(
            scope,
            __value,
            prefix.clone(),
            &__member_context,
            #options,
          )?
        }
        None => #missing,
      };
    });
    assignments.extend(quote! { #field_ident: #var, });
  }

  Ok(quote! {
    impl<'a> deno_core::webidl::WebIdlConverter<'a> for #ident {
      type Options = ();

      #[allow(unused_variables)]
      fn convert<C>(
        scope: &mut deno_core::v8::HandleScope<'a>,
        value: deno_core::v8::Local<'a, deno_core::v8::Value>,
        prefix: std::borrow::Cow<'static, str>,
        context: C,
        _options: &Self::Options,
      ) -> std::result::Result<Self, deno_core::webidl::WebIdlError>
      where
        C: Fn() -> std::borrow::Cow<'static, str>,
      {
        let __dictionary = deno_core::webidl::dictionary_object(
          value, &prefix, &context, #name,
        )?;
        #statements
        Ok(Self { #assignments })
      }
    }
  })
}

fn enumeration(
  ident: &Ident,
  data: &syn::DataEnum,
) -> syn::Result<TokenStream> {
  let name = ident.to_string();
  let mut arms = TokenStream::new();
  for variant in &data.variants {
    if !matches!(variant.fields, Fields::Unit) {
      return Err(syn::Error::new(
        variant.span(),
        "#[webidl(enum)] requires unit variants",
      ));
    }
    let attrs = member_attrs(&variant.attrs)?;
    if attrs.default.is_some() || attrs.required || !attrs.options.is_empty() {
      return Err(syn::Error::new(
        variant.span(),
        "only `rename` is allowed on an enum variant",
      ));
    }
    let value = attrs
      .rename
      .unwrap_or_else(|| stringcase::kebab_case(&variant.ident.to_string()));
    let variant_ident = &variant.ident;
    arms.extend(quote! { #value => Ok(Self::#variant_ident), });
  }

  Ok(quote! {
    impl<'a> deno_core::webidl::WebIdlConverter<'a> for #ident {
      type Options = ();

      fn convert<C>(
        scope: &mut deno_core::v8::HandleScope<'a>,
        value: deno_core::v8::Local<'a, deno_core::v8::Value>,
        prefix: std::borrow::Cow<'static, str>,
        context: C,
        _options: &Self::Options,
      ) -> std::result::Result<Self, deno_core::webidl::WebIdlError>
      where
        C: Fn() -> std::borrow::Cow<'static, str>,
      {
        let __value = <String as deno_core::webidl::WebIdlConverter<'a>>::convert(
          scope,
          value,
          prefix.clone(),
          &context,
          &Default::default(),
        )?;
        match __value.as_str() {
          #arms
          _ => Err(deno_core::webidl::WebIdlError::new(
            prefix,
            context(),
            deno_core::webidl::WebIdlErrorKind::Other(
              std::borrow::Cow::Owned(format!(
                "can not be converted to '{}' because '{}' is not a valid value",
                #name, __value,
              )),
            ),
          )),
        }
      }
    }
  })
}

#[cfg(test)]
mod tests {
  use std::path::PathBuf;

  use quote::ToTokens;
  use syn::DeriveInput;
  use syn::File;
  use syn::Item;

  fn expand(source: &str) -> Result<String, syn::Error> {
    let input = syn::parse_str::<syn::DeriveInput>(source)?;
    let tokens = super::derive_webidl(input)?;
    let tree = syn::parse2::<File>(tokens).expect("generated invalid Rust");
    Ok(prettyplease::unparse(&tree))
  }

  fn error_message(source: &str) -> String {
    match expand(source) {
      Ok(_) => panic!("expected an error for: {source}"),
      Err(err) => err.to_string(),
    }
  }

  #[test]
  fn misuse_is_a_compile_error() {
    assert_eq!(
      error_message("struct Foo { a: bool }"),
      "#[derive(WebIDL)] requires `#[webidl(dictionary)]` on a struct or `#[webidl(enum)]` on an enum"
    );
    assert_eq!(
      error_message("#[webidl(dictionary)] struct Foo(bool);"),
      "#[webidl(dictionary)] requires a struct with named fields"
    );
    assert_eq!(
      error_message("#[webidl(dictionary)] struct Foo;"),
      "#[webidl(dictionary)] requires a struct with named fields"
    );
    assert_eq!(
      error_message("#[webidl(enum)] enum Foo { A(u8) }"),
      "#[webidl(enum)] requires unit variants"
    );
    assert_eq!(
      error_message(
        "#[webidl(dictionary)] struct Foo { #[webidl(bogus)] a: bool }"
      ),
      "unknown webidl attribute"
    );
    assert_eq!(
      error_message("#[webidl(dictionary)] struct Foo { #[webidl(required, default = false)] a: bool }"),
      "a member can not be both `required` and have a `default`"
    );
    assert_eq!(
      error_message("#[webidl(dictionary)] struct Foo<T> { a: T }"),
      "#[derive(WebIDL)] does not support generic types"
    );
  }

  #[test]
  fn enum_expansion_uses_kebab_case_and_rename() {
    let out = expand(
      r#"#[webidl(enum)] enum Foo { FooBar, #[webidl(rename = "baz")] Qux }"#,
    )
    .unwrap();
    assert!(out.contains(r#""foo-bar" => Ok(Self::FooBar)"#), "{out}");
    assert!(out.contains(r#""baz" => Ok(Self::Qux)"#), "{out}");
  }

  #[test]
  fn dictionary_members_are_sorted_by_key() {
    let out =
      expand("#[webidl(dictionary)] struct Foo { zed: bool, foo_bar: bool }")
        .unwrap();
    let foo = out.find("\"fooBar\"").unwrap();
    let zed = out.find("\"zed\"").unwrap();
    assert!(foo < zed, "{out}");
  }

  #[testing_macros::fixture("webidl/test_cases/*.rs")]
  fn test_proc_macro_output(input: PathBuf) {
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
      syn::parse_str::<File>(&source).expect("Failed to parse Rust file");
    let mut expected_out = vec![];
    for item in file.items {
      let derive = match &item {
        Item::Struct(item) => Some(DeriveInput::from(item.clone())),
        Item::Enum(item) => Some(DeriveInput::from(item.clone())),
        _ => None,
      };
      let Some(input) = derive else { continue };
      let has_derive = input.attrs.iter().any(|attr| {
        attr.path().is_ident("derive")
          && attr.to_token_stream().to_string().contains("WebIDL")
      });
      if has_derive {
        let tokens = super::derive_webidl(input).expect("Failed to expand");
        let tree = syn::parse2::<File>(tokens).unwrap();
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
}
