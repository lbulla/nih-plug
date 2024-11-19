use proc_macro::TokenStream;
use syn::spanned::Spanned;
use quote::quote;

pub fn derive_property_dispatcher_impl(input: TokenStream) -> TokenStream {
    let ast = syn::parse_macro_input!(input as syn::DeriveInput);

    let struct_name = &ast.ident;
    let variants = match ast.data {
        syn::Data::Enum(syn::DataEnum { variants, .. }) => variants,
        _ => {
            return syn::Error::new(
                ast.span(),
                "Deriving `PropertyDispatcherImpl` is only supported on enums."
            )
                .to_compile_error()
                .into()
        }
    };

    let mut id_to_info_tokens = Vec::new();
    let mut id_to_get_tokens = Vec::new();
    let mut id_to_set_tokens = Vec::new();

    for (_variant_idx, variant) in variants.iter().enumerate() {
        let variant_ident = &variant.ident;

        id_to_info_tokens.push(quote! {
            <#variant_ident as seal::PropertyImpl<P>>::ID => #variant_ident::info(
                wrapper,
                in_scope,
                in_element,
                out_data_size,
                out_writable
            ),
        });

        id_to_get_tokens.push(quote! {
            <#variant_ident as seal::PropertyImpl<P>>::ID => #variant_ident::get(
                wrapper,
                in_scope,
                in_element,
                out_data,
                io_data_size,
            ),
        });

        id_to_set_tokens.push(quote! {
            <#variant_ident as seal::PropertyImpl<P>>::ID => #variant_ident::set(
                wrapper,
                in_scope,
                in_element,
                in_data,
                in_data_size
            ),
        });
    }

    quote! {
        impl<P: AuPlugin> PropertyDispatcherImpl<P> for #struct_name {
            fn info(
                id: au_sys::AudioUnitPropertyID,
                wrapper: &Wrapper<P>,
                in_scope: au_sys::AudioUnitScope,
                in_element: au_sys::AudioUnitElement,
                out_data_size: *mut au_sys::UInt32,
                out_writable: *mut au_sys::Boolean,
            ) -> au_sys::OSStatus {
                match id {
                    #(#id_to_info_tokens)*
                    _ => au_sys::kAudioUnitErr_PropertyNotInUse,
                }
            }

            fn get(
                id: au_sys::AudioUnitPropertyID,
                wrapper: &Wrapper<P>,
                in_scope: au_sys::AudioUnitScope,
                in_element: au_sys::AudioUnitElement,
                out_data: *mut c_void,
                io_data_size: *mut au_sys::UInt32,
            ) -> au_sys::OSStatus {
                match id {
                    #(#id_to_get_tokens)*
                    _ => au_sys::kAudioUnitErr_PropertyNotInUse,
                }
            }

            fn set(
                id: au_sys::AudioUnitPropertyID,
                wrapper: &mut Wrapper<P>,
                in_scope: au_sys::AudioUnitScope,
                in_element: au_sys::AudioUnitElement,
                in_data: *const c_void,
                in_data_size: au_sys::UInt32,
            ) -> au_sys::OSStatus {
                match id {
                    #(#id_to_set_tokens)*
                    _ => au_sys::kAudioUnitErr_PropertyNotInUse,
                }
            }
        }
    }.into()
}
