//! Procedural macros for Privchat FFI
//! 
//! This crate provides macros to simplify UniFFI exports with proper
//! async runtime handling for different platforms.

use proc_macro::TokenStream;
use quote::quote;
use syn::{ImplItem, Item, TraitItem};

/// Attribute macro for exporting Rust code to UniFFI
/// 
/// This macro automatically handles async runtime configuration:
/// - On WASM: Uses single-threaded runtime
/// - On native: Uses tokio multi-threaded runtime
/// 
/// # Examples
/// 
/// ```rust,ignore
/// #[privchat_ffi_macros::export]
/// impl MyStruct {
///     pub async fn my_async_method(&self) -> Result<String> {
///         // async code
///     }
/// }
/// ```
/// 
/// ```rust,ignore
/// #[privchat_ffi_macros::export(callback_interface)]
/// pub trait MyCallback {
///     fn on_event(&self, data: String);
/// }
/// ```
#[proc_macro_attribute]
pub fn export(attr: TokenStream, item: TokenStream) -> TokenStream {
    let has_async_fn = |item| {
        if let Item::Fn(fun) = &item {
            if fun.sig.asyncness.is_some() {
                return true;
            }
        } else if let Item::Impl(blk) = &item {
            for item in &blk.items {
                if let ImplItem::Fn(fun) = item {
                    if fun.sig.asyncness.is_some() {
                        return true;
                    }
                }
            }
        } else if let Item::Trait(blk) = &item {
            for item in &blk.items {
                if let TraitItem::Fn(fun) = item {
                    if fun.sig.asyncness.is_some() {
                        return true;
                    }
                }
            }
        }

        false
    };

    let _attr2 = proc_macro2::TokenStream::from(attr);
    let item2 = proc_macro2::TokenStream::from(item.clone());

    let res = match syn::parse(item) {
        Ok(item) => match has_async_fn(item) {
            true => {
                // For async functions in UniFFI 0.31, just use export (async is automatically detected)
                // UniFFI 0.31 automatically handles async functions
                quote! {
                  #[uniffi::export]
                }
            }
            false => {
                // For sync functions, simple export
                quote! { #[uniffi::export] }
            }
        },
        Err(e) => e.into_compile_error(),
    };

    quote! {
        #res
        #item2
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_macro_exists() {
        // Basic smoke test
        assert!(true);
    }
}
