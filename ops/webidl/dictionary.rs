// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use std::collections::HashSet;

use proc_macro2::TokenStream;
use quote::format_ident;
use quote::quote;
use syn::punctuated::Punctuated;
use syn::DataStruct;
use syn::DeriveInput;
use syn::Expr;
use syn::Fields;
use syn::Ident;
use syn::LitStr;
use syn::MetaNameValue;
use syn::Token;
use syn::Type;

/// One member of a dictionary, parsed from a struct field.
struct Member {
  field: Ident,
  ty: Type,
  /// The JS property key: the camelCase field name or `#[webidl(rename)]`.
  key: String,
  required: bool,
  default: Option<Expr>,
  is_option: bool,
  /// `#[options(name = value)]` settings for the member's converter options.
  options: Vec<(Ident, Expr)>,
}

pub(crate) fn derive(
  input: &DeriveInput,
  data: &DataStruct,
) -> syn::Result<TokenStream> {
  let Fields::Named(named) = &data.fields else {
    return Err(syn::Error::new_spanned(
      &input.ident,
      "#[webidl(dictionary)] requires a struct with named fields",
    ));
  };
  let name = &input.ident;
  let name_str = name.to_string();

  let mut members = vec![];
  let mut keys = HashSet::new();
  for field in &named.named {
    let member = parse_member(field)?;
    if !keys.insert(member.key.clone()) {
      return Err(syn::Error::new_spanned(
        &member.field,
        format!("duplicate WebIDL member key '{}'", member.key),
      ));
    }
    members.push(member);
  }

  // WebIDL reads dictionary members in lexicographic order of their keys, so
  // getters run in that order regardless of the field declaration order.
  let mut order: Vec<usize> = (0..members.len()).collect();
  order.sort_by(|a, b| {
    members[*a]
      .key
      .encode_utf16()
      .cmp(members[*b].key.encode_utf16())
  });

  let reads = order
    .iter()
    .map(|&index| read_member(&name_str, index, &members[index]));
  let fields = members.iter().enumerate().map(|(index, member)| {
    let field = &member.field;
    let var = member_var(index);
    quote!(#field: #var)
  });

  Ok(quote! {
    impl<'a> deno_core::webidl::WebIdlConverter<'a> for #name {
      type Options = ();

      fn convert<C>(
        scope: &mut deno_core::v8::HandleScope<'a>,
        value: deno_core::v8::Local<'a, deno_core::v8::Value>,
        prefix: std::borrow::Cow<'static, str>,
        context: C,
        options: &Self::Options,
      ) -> std::result::Result<Self, deno_core::webidl::WebIdlError>
      where
        C: Fn() -> std::borrow::Cow<'static, str>,
      {
        let _ = options;
        let dictionary = deno_core::webidl::dictionary_object(
          value,
          #name_str,
          &prefix,
          &context,
        )?;
        #(#reads)*
        std::result::Result::Ok(#name {
          #(#fields,)*
        })
      }
    }
  })
}

fn member_var(index: usize) -> Ident {
  format_ident!("__webidl_member_{}", index)
}

fn parse_member(field: &syn::Field) -> syn::Result<Member> {
  let Some(ident) = field.ident.clone() else {
    return Err(syn::Error::new_spanned(
      field,
      "#[webidl(dictionary)] requires named fields",
    ));
  };
  // `r#type` is the member `type`.
  let raw = ident.to_string();
  let name = raw.strip_prefix("r#").unwrap_or(&raw);
  let mut key = stringcase::camel_case(name);
  let mut required = false;
  let mut default = None;
  let mut options = vec![];

  for attr in &field.attrs {
    if attr.path().is_ident("webidl") {
      attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("required") {
          required = true;
          Ok(())
        } else if meta.path.is_ident("rename") {
          let lit: LitStr = meta.value()?.parse()?;
          key = lit.value();
          Ok(())
        } else if meta.path.is_ident("default") {
          default = Some(meta.value()?.parse::<Expr>()?);
          Ok(())
        } else {
          Err(meta.error(
            "unknown #[webidl(...)] key on a dictionary member, expected `required`, `rename`, or `default`",
          ))
        }
      })?;
    } else if attr.path().is_ident("options") {
      let pairs = attr.parse_args_with(
        Punctuated::<MetaNameValue, Token![,]>::parse_terminated,
      )?;
      for pair in pairs {
        let Some(name) = pair.path.get_ident().cloned() else {
          return Err(syn::Error::new_spanned(
            &pair.path,
            "#[options] expects `name = value` pairs",
          ));
        };
        options.push((name, pair.value));
      }
    }
  }

  let is_option = is_option(&field.ty);
  Ok(Member {
    field: ident,
    ty: field.ty.clone(),
    key,
    required,
    default,
    is_option,
    options,
  })
}

/// Whether the type's last path segment is `Option`.
fn is_option(ty: &Type) -> bool {
  matches!(ty, Type::Path(path) if path
    .path
    .segments
    .last()
    .is_some_and(|segment| segment.ident == "Option"))
}

/// Generates the code that reads one member, binding it to its variable.
fn read_member(dictionary: &str, index: usize, member: &Member) -> TokenStream {
  let var = member_var(index);
  let ty = &member.ty;
  let key = &member.key;

  let options = if member.options.is_empty() {
    quote! {
      let options: <#ty as deno_core::webidl::WebIdlConverter<'a>>::Options =
        std::default::Default::default();
    }
  } else {
    let settings = member
      .options
      .iter()
      .map(|(name, value)| quote!(options.#name = #value;));
    quote! {
      let mut options: <#ty as deno_core::webidl::WebIdlConverter<'a>>::Options =
        std::default::Default::default();
      #(#settings)*
    }
  };

  let missing_error = quote! {
    std::result::Result::Err(deno_core::webidl::missing_dictionary_member(
      &prefix, &context, #dictionary, #key,
    ))
  };
  let missing = if member.required {
    quote!(return #missing_error)
  } else if let Some(default) = &member.default {
    quote!(#default)
  } else if member.is_option {
    quote!(::std::option::Option::None)
  } else {
    quote!(return #missing_error)
  };

  // Built at macro time so the generated call is short and formats the same
  // in every build. Braces in the names are escaped for `format!`.
  let context_fmt = format!(
    "'{}' of '{}' ({{}})",
    key.replace('{', "{{").replace('}', "}}"),
    dictionary.replace('{', "{{").replace('}', "}}"),
  );

  quote! {
    let #var: #ty = match deno_core::webidl::dictionary_member(
      scope,
      dictionary,
      #key,
      &prefix,
      &context,
    )? {
      std::option::Option::Some(value) => {
        #options
        let member_context = || -> std::borrow::Cow<'static, str> {
          std::format!(#context_fmt, context()).into()
        };
        <#ty as deno_core::webidl::WebIdlConverter<'a>>::convert(
          scope,
          value,
          prefix.clone(),
          member_context,
          &options,
        )?
      }
      std::option::Option::None => #missing,
    };
  }
}
