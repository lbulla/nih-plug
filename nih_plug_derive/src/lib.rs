use proc_macro::TokenStream;

#[cfg(all(feature = "au", target_os = "macos"))]
mod au;
mod enums;
mod params;

#[cfg(all(feature = "au", target_os = "macos"))]
#[proc_macro_derive(PropertyDispatcherImpl)]
pub fn au_derive_property_dispatcher_impl(input: TokenStream) -> TokenStream {
    au::derive_property_dispatcher_impl(input)
}

/// Derive the `Enum` trait for simple enum parameters. See `EnumParam` for more information.
#[proc_macro_derive(Enum, attributes(name, id))]
pub fn derive_enum(input: TokenStream) -> TokenStream {
    enums::derive_enum(input)
}

/// Derive the `Params` trait for your plugin's parameters struct. See the `Plugin` trait.
#[proc_macro_derive(Params, attributes(id, persist, nested))]
pub fn derive_params(input: TokenStream) -> TokenStream {
    params::derive_params(input)
}
