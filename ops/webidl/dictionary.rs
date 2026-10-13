// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use proc_macro2::Ident;
use proc_macro2::TokenStream;
use quote::format_ident;
use quote::quote;
use syn::DataStruct;
use syn::DeriveInput;
use syn::Expr;
use syn::ExprLit;
use syn::Fields;
use syn::Lit;
use syn::Type;

use super::is_option;
use super::parse_args;
use super::unraw;

/// A dictionary member, read from the JavaScript object by `key`.
struct Member {
  ident: Ident,
  ty: Type,
  key: String,
  default: Option<Expr>,
  required: bool,
  options: Vec<(Ident, Expr)>,
}

pub(super) fn derive(
  input: &DeriveInput,
  data: &DataStruct,
) -> Result<TokenStream, syn::Error> {
  let Fields::Named(fields) = &data.fields else {
    return Err(syn::Error::new_spanned(
      &input.ident,
      "#[webidl(dictionary)] can only be used on a struct with named fields",
    ));
  };
  if !input.generics.params.is_empty() {
    return Err(syn::Error::new_spanned(
      &input.generics,
      "#[webidl(dictionary)] does not support generic structs",
    ));
  }

  let mut members = Vec::new();
  for field in &fields.named {
    let ident = field.ident.clone().expect("named field");
    let mut key = stringcase::camel_case(&unraw(&ident));
    let mut default = None;
    let mut required = false;
    let mut options = Vec::new();
    let mut default_attr = None;
    let mut seen: Vec<String> = Vec::new();

    for attr in &field.attrs {
      if attr.path().is_ident("webidl") {
        for arg in parse_args(attr)? {
          let name = arg.name.to_string();
          if seen.contains(&name) {
            return Err(syn::Error::new_spanned(
              &arg.name,
              format!("duplicate #[webidl] field attribute `{name}`"),
            ));
          }
          seen.push(name.clone());
          match (name.as_str(), arg.value) {
            ("required", None) => required = true,
            ("rename", Some(value)) => {
              key = string_literal(&value).ok_or_else(|| {
                syn::Error::new_spanned(&value, "expected a string literal")
              })?;
            }
            ("default", Some(value)) => {
              default = Some(value);
              default_attr = Some(attr);
            }
            _ => {
              return Err(syn::Error::new_spanned(
                &arg.name,
                format!("unknown #[webidl] field attribute `{name}`"),
              ))
            }
          }
        }
      } else if attr.path().is_ident("options") {
        for arg in parse_args(attr)? {
          let Some(value) = arg.value else {
            return Err(syn::Error::new_spanned(
              &arg.name,
              "expected #[options(field = value)]",
            ));
          };
          if options.iter().any(|(name, _)| *name == arg.name) {
            return Err(syn::Error::new_spanned(
              &arg.name,
              format!("duplicate #[options] field `{}`", arg.name),
            ));
          }
          options.push((arg.name, value));
        }
      }
    }

    if let (true, Some(attr)) = (required, default_attr) {
      return Err(syn::Error::new_spanned(
        attr,
        "#[webidl(required)] can not be combined with #[webidl(default)]",
      ));
    }
    members.push(Member {
      ident,
      ty: field.ty.clone(),
      key,
      default,
      required,
      options,
    });
  }

  // Keys must be unique, otherwise one member would silently shadow another.
  let mut keys: Vec<&str> = members.iter().map(|m| m.key.as_str()).collect();
  keys.sort_unstable();
  if let Some(pair) = keys.windows(2).find(|w| w[0] == w[1]) {
    return Err(syn::Error::new_spanned(
      &input.ident,
      format!("duplicate dictionary key '{}'", pair[0]),
    ));
  }

  // WebIDL reads members in the lexicographic order of their keys, compared
  // by UTF-16 code units as JavaScript does.
  members.sort_by(|a, b| a.key.encode_utf16().cmp(b.key.encode_utf16()));

  Ok(generate(input, &members))
}

fn string_literal(expr: &Expr) -> Option<String> {
  match expr {
    Expr::Lit(ExprLit {
      lit: Lit::Str(s), ..
    }) => Some(s.value()),
    _ => None,
  }
}

fn generate(input: &DeriveInput, members: &[Member]) -> TokenStream {
  let name = &input.ident;
  let dict_name = name.to_string();

  let reads = members.iter().enumerate().map(|(index, member)| {
    let var = format_ident!("__webidl_member_{}", index);
    let options_var = format_ident!("__webidl_options_{}", index);
    let ty = &member.ty;
    let key = &member.key;
    let option_fields = member.options.iter().map(|(field, _)| field);
    let option_values = member.options.iter().map(|(_, value)| value);
    let options_setup = if member.options.is_empty() {
      quote! {
        let #options_var = <#ty as deno_core::webidl::WebIdlConverter<'a>>::Options::default();
      }
    } else {
      quote! {
        let mut #options_var = <#ty as deno_core::webidl::WebIdlConverter<'a>>::Options::default();
        #(#options_var.#option_fields = #option_values;)*
      }
    };
    let missing = if let Some(default) = &member.default {
      quote!(#default)
    } else if is_option(ty) && !member.required {
      quote!(None)
    } else {
      quote! {
        return Err(deno_core::webidl::dictionary_missing_member(
          prefix.clone(),
          &context,
          #dict_name,
          #key,
        ))
      }
    };
    quote! {
      #options_setup
      let #var: #ty = match deno_core::webidl::dictionary_member::<#ty, _>(
        scope,
        dictionary,
        #dict_name,
        #key,
        prefix.clone(),
        &context,
        &#options_var,
      )? {
        Some(value) => value,
        None => #missing,
      };
    }
  });

  let field_idents = members.iter().map(|m| &m.ident);
  let field_vars =
    (0..members.len()).map(|i| format_ident!("__webidl_member_{}", i));

  quote! {
    impl<'a> deno_core::webidl::WebIdlConverter<'a> for #name {
      type Options = ();

      fn convert<C>(
        scope: &mut deno_core::v8::HandleScope<'a>,
        value: deno_core::v8::Local<'a, deno_core::v8::Value>,
        prefix: std::borrow::Cow<'static, str>,
        context: C,
        _options: &Self::Options,
      ) -> Result<Self, deno_core::webidl::WebIdlError>
      where
        C: Fn() -> std::borrow::Cow<'static, str>,
      {
        let dictionary = deno_core::webidl::dictionary_object(
          value,
          #dict_name,
          &prefix,
          &context,
        )?;
        #(#reads)*
        Ok(#name {
          #(#field_idents: #field_vars,)*
        })
      }
    }
  }
}
